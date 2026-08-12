use std::fs;
use std::ops::Deref;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};

mod support;
use support::{
    ManagedStateFixture, binary, dof, full_mode, mode, output, set_mode, stderr, stdout,
};

#[test]
fn apply_help_describes_a_zero_argument_command() {
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
    fs::create_dir(fixture.workspace.join("no-home")).unwrap();

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
fn empty_workspace_applies_nothing() {
    let fixture = Fixture::new();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(stdout(&result), "applied: 0\nunchanged: 0\n");
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
  middle-disabled: false
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

    let expected = format!(
        "alpha:{}\nzeta:{}\nalpha:{}\nzeta:{}\n",
        fixture.workspace.join("alpha").display(),
        fixture.workspace.join("zeta").display(),
        fixture.workspace.join("alpha").display(),
        fixture.workspace.join("zeta").display(),
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        expected
    );
}

#[test]
fn feature_script_runs_with_a_relative_home() {
    let fixture = Fixture::new();
    fixture.write_apply_script(
        "default",
        "#!/bin/sh\nprintf 'ran\n' > \"$OLDPWD/script-ran\"\n",
    );
    let current_directory = fixture.root.path();
    let relative_home = fixture.home.strip_prefix(current_directory).unwrap();

    let result = output(
        Command::new(binary())
            .current_dir(current_directory)
            .env("HOME", relative_home)
            .env("OLDPWD", &fixture.home)
            .arg("apply"),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-ran")).unwrap(),
        "ran\n"
    );
}

#[test]
fn failing_feature_script_stops_later_scripts_after_file_sync() {
    let fixture = Fixture::new();
    let zeta = fixture.feature_home("zeta");
    fs::write(zeta.join("managed-before-scripts.txt"), "managed\n").unwrap();
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
    assert_eq!(
        fs::read_to_string(fixture.home.join("managed-before-scripts.txt")).unwrap(),
        "managed\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn later_scripts_are_revalidated_immediately_before_execution() {
    let fixture = Fixture::new();
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
    let _socket = UnixListener::bind(bad_home.join("socket")).unwrap();

    let result = output(dof(&special_fixture.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&special_fixture);
}

#[test]
fn destination_ancestor_symlinks_fail_during_preflight() {
    let fixture = Fixture::new();
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
  disabled: false
"#,
    );
    fixture.write_snippets(
        "default",
        "snippets:\n  .profile:\n    - 'enabled default'\n",
    );
    fixture.write_snippets(
        "explicit",
        "snippets:\n  .profile:\n    - 'enabled implicit'\n",
    );
    fixture.write_snippets(
        "disabled",
        "snippets:\n  .profile:\n    - 'must not be appended'\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    let contents = fs::read_to_string(fixture.home.join(".profile")).unwrap();
    assert!(contents.contains("enabled default"));
    assert!(contents.contains("enabled implicit"));
    assert!(!contents.contains("must not be appended"));
}

#[test]
fn multiple_features_can_append_snippets_to_one_target() {
    let fixture = Fixture::new();
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

fn prepare_preflight_sentinel(fixture: &Fixture) {
    let source = fixture.feature_home("a-safe");
    fs::write(source.join("sentinel"), "new\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();
}

fn config_with_disabled_bad_feature() -> &'static str {
    r#"repo:
  url: file:///dotfiles
  branch: main
features:
  z-bad: false
"#
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
    assert!(
        output.contains(&format!("applied: {applied}")),
        "missing applied count in stdout: {output:?}"
    );
    assert!(
        output.contains(&format!("unchanged: {unchanged}")),
        "missing unchanged count in stdout: {output:?}"
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
}
