use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

use support::{binary, create_unix_socket, dof, output, stderr, stdout};

#[test]
fn lint_help_describes_the_required_directory() {
    let help = output(Command::new(binary()).arg("lint").arg("--help"));
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("Usage: dof lint <DIRECTORY>"));

    let missing = output(Command::new(binary()).arg("lint"));
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("<DIRECTORY>"));

    let extra = output(
        Command::new(binary())
            .arg("lint")
            .arg("workspace")
            .arg("extra"),
    );
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument 'extra'"));
}

#[test]
fn lint_accepts_valid_features_and_ignores_workspace_root_content() {
    let fixture = Fixture::new();
    let home = fixture.feature_home("default");
    fs::create_dir_all(home.join("nested")).unwrap();
    fs::create_dir_all(home.join(".git")).unwrap();
    fs::write(home.join("nested/settings.yaml"), "value: true\n").unwrap();
    fs::write(home.join(".git/config"), "ordinary payload\n").unwrap();

    fixture.feature("missing-home");
    fs::create_dir_all(fixture.workspace.join("legacy/home")).unwrap();
    fs::create_dir_all(fixture.workspace.join(".git/objects")).unwrap();
    symlink(
        &fixture.workspace,
        fixture.workspace.join(".git/objects/cycle"),
    )
    .unwrap();

    let external = fixture.root.path().join("external-feature");
    fs::create_dir(&external).unwrap();
    symlink(&external, fixture.features.join("linked-feature")).unwrap();

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
    assert!(stdout(&result).is_empty());
}

#[test]
fn lint_accepts_a_root_symlink_to_a_directory() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.workspace).unwrap();
    let link = fixture.root.path().join("workspace-link");
    symlink(&fixture.workspace, &link).unwrap();

    let result = output(dof(&fixture.home).arg("lint").arg(link));

    assert!(result.status.success(), "{}", stderr(&result));
}

#[test]
fn lint_rejects_non_directory_features_paths_without_following_them() {
    let linked = Fixture::new();
    fs::create_dir_all(&linked.workspace).unwrap();
    let external = linked.root.path().join("external-features");
    fs::create_dir_all(external.join("default")).unwrap();
    symlink(&external, &linked.features).unwrap();
    let root_link = linked.root.path().join("workspace-link");
    symlink(&linked.workspace, &root_link).unwrap();

    let result = output(dof(&linked.home).arg("lint").arg(root_link));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("features directory"));
    assert!(stderr(&result).contains("not a real directory"));
    assert!(external.join("default").is_dir());

    let file = Fixture::new();
    fs::create_dir_all(&file.workspace).unwrap();
    fs::write(&file.features, "not a directory\n").unwrap();

    let result = file.lint();

    assert!(!result.status.success());
    assert!(stderr(&result).contains("features directory"));
    assert!(stderr(&result).contains("not a real directory"));
}

#[test]
fn lint_accepts_shared_directories_with_distinct_files_and_empty_directories() {
    let fixture = Fixture::new();
    let alpha = fixture.feature_home("alpha");
    let beta = fixture.feature_home("beta");
    fs::create_dir_all(alpha.join(".config/app/empty")).unwrap();
    fs::create_dir_all(beta.join(".config/app")).unwrap();
    fs::write(alpha.join(".config/app/alpha.yaml"), "alpha\n").unwrap();
    fs::write(beta.join(".config/app/beta.yaml"), "beta\n").unwrap();

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
}

#[test]
fn lint_accepts_an_empty_workspace() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.workspace).unwrap();

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
    assert!(stdout(&result).is_empty());
}

#[test]
fn lint_rejects_missing_paths_and_regular_files() {
    let fixture = Fixture::new();
    let missing = fixture.workspace.join("missing");
    let result = output(dof(&fixture.home).arg("lint").arg(&missing));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to inspect workspace"));

    let file = fixture.root.path().join("not-a-directory.yaml");
    fs::write(&file, "value: true\n").unwrap();
    let result = output(dof(&fixture.home).arg("lint").arg(&file));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a real directory"));
}

#[test]
fn lint_rejects_the_reserved_feature_name() {
    let fixture = Fixture::new();
    fixture.feature_home(".dof");

    let result = fixture.lint();

    assert!(!result.status.success());
    assert!(stderr(&result).contains("feature '.dof' is forbidden"));
}

#[test]
fn lint_rejects_dof_state_in_a_home_payload() {
    let fixture = Fixture::new();
    let home = fixture.feature_home("default");
    fs::create_dir_all(home.join(".dof")).unwrap();
    fs::write(home.join(".dof/config.yaml"), "protected\n").unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains("feature 'default'"));
    assert!(error.contains("forbidden home payload '.dof'"));
}

#[test]
fn lint_rejects_duplicate_files_with_deterministic_diagnostics() {
    let fixture = Fixture::new();
    let zeta = fixture.feature_home("zeta");
    let alpha = fixture.feature_home("alpha");
    fs::write(zeta.join(".bashrc"), "zeta\n").unwrap();
    fs::write(alpha.join(".bashrc"), "alpha\n").unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    assert!(
        stderr(&result)
            .contains("destination '.bashrc' is a file in both feature 'alpha' and feature 'zeta'")
    );
}

#[test]
fn lint_rejects_file_directory_and_ancestor_collisions() {
    let fixture = Fixture::new();
    let alpha = fixture.feature_home("alpha");
    let beta = fixture.feature_home("beta");
    fs::create_dir_all(&alpha).unwrap();
    fs::write(alpha.join(".config"), "file\n").unwrap();
    fs::create_dir_all(beta.join(".config/app")).unwrap();
    fs::write(beta.join(".config/app/settings.yaml"), "nested\n").unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    assert!(stderr(&result).contains(
        "destination '.config' is a file in feature 'alpha' and a directory in feature 'beta'"
    ));
}

#[test]
fn lint_rejects_source_symlinks() {
    let fixture = Fixture::new();
    let home = fixture.feature_home("default");
    let external = fixture.root.path().join("external");
    fs::write(&external, "outside\n").unwrap();
    symlink(&external, home.join(".bashrc")).unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains("home entry '.bashrc' in feature 'default' is a symlink"));
    assert!(error.contains("source symlinks are not supported"));
}

#[test]
fn lint_rejects_special_source_files() {
    let fixture = Fixture::new();
    let home = fixture.feature_home("default");
    let socket_path = home.join("agent.sock");
    let _socket = create_unix_socket(&socket_path);

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains("home entry 'agent.sock' in feature 'default'"));
    assert!(error.contains("unsupported file type"));
}

#[test]
fn lint_requires_home_to_be_a_real_directory_when_present() {
    let fixture = Fixture::new();
    let feature = fixture.feature("default");
    let external = fixture.root.path().join("external-home");
    fs::create_dir_all(&feature).unwrap();
    fs::create_dir(&external).unwrap();
    symlink(&external, feature.join("home")).unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains("home path for feature 'default'"));
    assert!(error.contains("is not a real directory"));
}

#[test]
fn lint_accepts_an_owner_executable_apply_script_with_a_shebang() {
    let fixture = Fixture::new();
    fixture.write_apply_script("default", "#!/bin/sh\nexit 0\n", 0o700);

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
}

#[test]
fn lint_requires_apply_scripts_to_be_executable() {
    let fixture = Fixture::new();
    fixture.write_apply_script("default", "#!/bin/sh\nexit 0\n", 0o644);

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(
        error.contains("apply script for feature 'default'"),
        "{error}"
    );
    assert!(error.contains("not executable"), "{error}");
}

#[test]
fn lint_requires_apply_scripts_to_begin_with_a_shebang() {
    let fixture = Fixture::new();
    fixture.write_apply_script("default", "echo no-shebang\n", 0o700);

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(
        error.contains("apply script for feature 'default'"),
        "{error}"
    );
    assert!(error.contains("must begin with a shebang"), "{error}");
}

#[test]
fn lint_rejects_non_regular_apply_paths() {
    let directory = Fixture::new();
    fs::create_dir_all(directory.feature("default").join("apply")).unwrap();
    let result = directory.lint();
    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a regular file"));

    let link = Fixture::new();
    let external = link.root.path().join("external-apply");
    fs::write(&external, "#!/bin/sh\nexit 0\n").unwrap();
    let feature = link.feature("default");
    symlink(&external, feature.join("apply")).unwrap();
    let result = link.lint();
    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a regular file"));

    let special = Fixture::new();
    let feature = special.feature("default");
    let _socket = create_unix_socket(&feature.join("apply"));
    let result = special.lint();
    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a regular file"));
}

#[test]
fn lint_accepts_valid_multiline_snippets_and_shared_targets() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "alpha",
        r#"snippets:
  .bashrc:
    - 'export EDITOR=vim'
    - |-
      if command -v mise >/dev/null 2>&1; then
        eval "$(mise activate bash)"
      fi
"#,
    );
    fixture.write_snippets("beta", "snippets:\n  .bashrc:\n    - 'export PAGER=less'\n");

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
}

#[test]
fn lint_rejects_invalid_snippets_schemas() {
    let cases = [
        ("root sequence", "- snippets\n- are not a mapping\n"),
        ("missing snippets", "unrelated: {}\n"),
        ("snippets sequence", "snippets: []\n"),
        (
            "non-array target value",
            "snippets:\n  .bashrc: export EDITOR=vim\n",
        ),
        (
            "non-string array member",
            "snippets:\n  .bashrc:\n    - export EDITOR=vim\n    - 42\n",
        ),
    ];

    for (name, yaml) in cases {
        let fixture = Fixture::new();
        fixture.write_snippets("default", yaml);

        let result = fixture.lint();

        assert!(!result.status.success(), "{name} unexpectedly passed");
        assert!(
            stderr(&result).contains("snippets"),
            "{name} did not identify snippets.yaml: {}",
            stderr(&result)
        );
    }
}

#[test]
fn lint_rejects_non_regular_snippets_files() {
    let directory = Fixture::new();
    fs::create_dir_all(directory.feature("default").join("snippets.yaml")).unwrap();
    let result = directory.lint();
    assert!(!result.status.success());
    assert!(stderr(&result).contains("snippets"));
    assert!(stderr(&result).contains("not a regular file"));

    let link = Fixture::new();
    let external = link.root.path().join("external-snippets.yaml");
    fs::write(&external, "snippets: {}\n").unwrap();
    let feature = link.feature("default");
    symlink(&external, feature.join("snippets.yaml")).unwrap();
    let result = link.lint();
    assert!(!result.status.success());
    assert!(stderr(&result).contains("snippets"));
    assert!(stderr(&result).contains("not a regular file"));
}

#[test]
fn lint_rejects_unsafe_snippet_targets() {
    let cases = [
        ("absolute", "/tmp/dof-target"),
        ("parent traversal", "../outside-home"),
        ("dof state", ".dof/config.yaml"),
    ];

    for (name, target) in cases {
        let fixture = Fixture::new();
        fixture.write_snippets(
            "default",
            &format!("snippets:\n  {target}:\n    - managed\n"),
        );

        let result = fixture.lint();

        assert!(!result.status.success(), "unsafe {name} target passed lint");
        assert!(stderr(&result).contains(target), "{}", stderr(&result));
    }
}

#[test]
fn lint_rejects_copy_and_snippet_ownership_of_the_same_file() {
    let same_feature = Fixture::new();
    let home = same_feature.feature_home("default");
    fs::write(home.join(".bashrc"), "copied\n").unwrap();
    same_feature.write_snippets(
        "default",
        "snippets:\n  .bashrc:\n    - 'snippet managed'\n",
    );

    let result = same_feature.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".bashrc"), "{error}");
    assert!(error.contains("default"), "{error}");

    let different_features = Fixture::new();
    let home = different_features.feature_home("copy-owner");
    fs::write(home.join(".profile"), "copied\n").unwrap();
    different_features.write_snippets(
        "snippet-owner",
        "snippets:\n  .profile:\n    - 'snippet managed'\n",
    );

    let result = different_features.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".profile"), "{error}");
    assert!(error.contains("copy-owner"), "{error}");
    assert!(error.contains("snippet-owner"), "{error}");

    let aliased = Fixture::new();
    let home = aliased.feature_home("copy-owner");
    fs::create_dir(home.join("nested")).unwrap();
    fs::write(home.join("nested/target"), "copied\n").unwrap();
    aliased.write_snippets(
        "snippet-owner",
        "snippets:\n  nested//./target:\n    - 'snippet managed'\n",
    );

    let result = aliased.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains("nested/target"), "{error}");
    assert!(error.contains("copy-owner"), "{error}");
    assert!(error.contains("snippet-owner"), "{error}");

    let structural = Fixture::new();
    let home = structural.feature_home("copy-owner");
    fs::write(home.join(".config"), "copied file\n").unwrap();
    structural.write_snippets(
        "snippet-owner",
        "snippets:\n  .config/tool/settings:\n    - 'snippet managed'\n",
    );

    let result = structural.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".config"), "{error}");
    assert!(error.contains("copy-owner"), "{error}");
    assert!(error.contains("snippet-owner"), "{error}");
}

#[test]
fn lint_rejects_a_later_copy_file_that_contains_an_existing_snippet_target() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "a-snippet",
        "snippets:\n  .config/tool/settings:\n    - 'snippet managed'\n",
    );
    let home = fixture.feature_home("z-copy");
    fs::write(home.join(".config"), "copied file\n").unwrap();

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".config/tool/settings"), "{error}");
    assert!(error.contains("a-snippet"), "{error}");
    assert!(error.contains("z-copy"), "{error}");
}

#[test]
fn lint_rejects_a_later_snippet_target_that_contains_an_existing_snippet_target() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "alpha",
        "snippets:\n  .config/tool/settings:\n    - 'nested snippet'\n",
    );
    fixture.write_snippets("zeta", "snippets:\n  .config:\n    - 'ancestor snippet'\n");

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".config/tool/settings"), "{error}");
    assert!(error.contains("alpha"), "{error}");
    assert!(error.contains("zeta"), "{error}");
}

struct Fixture {
    root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
    features: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = home.join(".dof/workspace");
        let features = workspace.join("features");
        fs::create_dir(&home).unwrap();
        Self {
            root,
            home,
            workspace,
            features,
        }
    }

    fn feature(&self, feature: &str) -> PathBuf {
        let feature = self.features.join(feature);
        fs::create_dir_all(&feature).unwrap();
        feature
    }

    fn feature_home(&self, feature: &str) -> PathBuf {
        let home = self.feature(feature).join("home");
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn lint(&self) -> Output {
        output(dof(&self.home).arg("lint").arg(&self.workspace))
    }

    fn write_apply_script(&self, feature: &str, contents: &str, mode: u32) {
        let feature = self.feature(feature);
        fs::create_dir_all(&feature).unwrap();
        let path = feature.join("apply");
        fs::write(&path, contents).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn write_snippets(&self, feature: &str, contents: &str) {
        let feature = self.feature(feature);
        fs::create_dir_all(&feature).unwrap();
        fs::write(feature.join("snippets.yaml"), contents).unwrap();
    }
}
