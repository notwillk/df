#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
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

#[test]
fn lint_accepts_shared_and_nested_drop_ins_and_empty_roots() {
    let fixture = Fixture::new();
    fixture.write_drop_in("alpha", ".Brewfile.d", "00-base", b"tap \"base\"\n");
    fixture.write_drop_in("zeta", ".Brewfile.d", "99-z_1.foo-bar", b"brew \"zeta\"\n");
    fixture.write_drop_in(
        "nested",
        ".config/systemd/user/example.service.d/override.conf.d",
        "10-base",
        b"[Service]\n",
    );
    fs::create_dir_all(fixture.feature("empty").join("drop-ins")).unwrap();
    fixture.feature("missing-drop-ins");

    let result = fixture.lint();

    assert!(result.status.success(), "{}", stderr(&result));
    assert!(stdout(&result).is_empty());
}

#[test]
fn lint_reserves_drop_in_orders_globally_across_features() {
    let fixture = Fixture::new();
    fixture.write_drop_in("alpha", ".profile.d", "10-alpha", b"alpha\n");
    fixture.write_drop_in("zeta", ".profile.d", "10-zeta", b"zeta\n");

    let result = fixture.lint();

    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".profile"), "{error}");
    assert!(error.contains("alpha"), "{error}");
    assert!(error.contains("zeta"), "{error}");

    let duplicate_name = Fixture::new();
    duplicate_name.write_drop_in("alpha", ".bashrc.d", "20-shared", b"alpha\n");
    duplicate_name.write_drop_in("zeta", ".bashrc.d", "20-shared", b"zeta\n");
    let result = duplicate_name.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(".bashrc"), "{error}");
    assert!(error.contains("20-shared"), "{error}");
    assert!(error.contains("alpha"), "{error}");
    assert!(error.contains("zeta"), "{error}");
}

#[test]
fn lint_rejects_invalid_drop_in_tree_shapes() {
    let orphan = Fixture::new();
    let root = orphan.drop_ins("default");
    fs::write(root.join("10-orphan"), "orphan\n").unwrap();
    assert_drop_in_lint_failure(&orphan, "drop-ins root");

    let empty = Fixture::new();
    fs::create_dir_all(empty.drop_ins("default").join("empty-container")).unwrap();
    assert_drop_in_lint_failure(&empty, "empty-container");

    let missing_suffix = Fixture::new();
    let terminal = missing_suffix.drop_ins("default").join(".config/tool");
    fs::create_dir_all(&terminal).unwrap();
    fs::write(terminal.join("10-base"), "managed\n").unwrap();
    assert_drop_in_lint_failure(&missing_suffix, ".config/tool");

    let mixed = Fixture::new();
    let terminal = mixed.drop_ins("default").join(".profile.d");
    fs::create_dir_all(terminal.join("child")).unwrap();
    fs::write(terminal.join("10-base"), "managed\n").unwrap();
    fs::write(terminal.join("child/20-extra"), "extra\n").unwrap();
    assert_drop_in_lint_failure(&mixed, ".profile.d");
}

#[test]
fn lint_rejects_invalid_drop_in_names_and_contents() {
    for filename in [
        "0-short",
        "100-long",
        "10-Uppercase",
        "10-_leading",
        "10-has space",
    ] {
        let fixture = Fixture::new();
        fixture.write_drop_in("default", ".profile.d", filename, b"managed\n");
        assert_drop_in_lint_failure(&fixture, filename);
    }

    for (name, contents) in [
        ("empty", Vec::new()),
        ("invalid UTF-8", vec![0xff, b'\n']),
        ("NUL byte", b"managed\0\n".to_vec()),
        ("missing final newline", b"managed".to_vec()),
    ] {
        let fixture = Fixture::new();
        fixture.write_drop_in("default", ".profile.d", "10-base", &contents);
        let result = fixture.lint();
        assert!(!result.status.success(), "{name} unexpectedly passed lint");
        assert!(stderr(&result).contains("10-base"), "{}", stderr(&result));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn lint_rejects_non_utf8_drop_in_directories() {
    let non_utf8 = Fixture::new();
    let name = OsString::from_vec(vec![0xff, b'.', b'd']);
    let terminal = non_utf8.drop_ins("default").join(name);
    fs::create_dir_all(&terminal).unwrap();
    fs::write(terminal.join("10-base"), "managed\n").unwrap();
    let result = non_utf8.lint();
    assert!(!result.status.success());
}

#[test]
fn lint_rejects_symlinked_and_special_drop_in_sources() {
    let linked_fragment = Fixture::new();
    let external = linked_fragment.root.path().join("external-fragment");
    fs::write(&external, "external\n").unwrap();
    let terminal = linked_fragment.drop_ins("default").join(".profile.d");
    fs::create_dir_all(&terminal).unwrap();
    symlink(&external, terminal.join("10-base")).unwrap();
    assert_drop_in_lint_failure(&linked_fragment, "10-base");

    let special_fragment = Fixture::new();
    let terminal = special_fragment.drop_ins("default").join(".profile.d");
    fs::create_dir_all(&terminal).unwrap();
    let _socket = create_unix_socket(&terminal.join("10-base"));
    assert_drop_in_lint_failure(&special_fragment, "10-base");

    let linked_root = Fixture::new();
    let external = linked_root.root.path().join("external-drop-ins");
    fs::create_dir_all(external.join(".profile.d")).unwrap();
    fs::write(external.join(".profile.d/10-base"), "external\n").unwrap();
    symlink(&external, linked_root.feature("default").join("drop-ins")).unwrap();
    assert_drop_in_lint_failure(&linked_root, "drop-ins");
}

#[test]
fn lint_rejects_drop_in_copy_snippet_and_structural_collisions() {
    let copy = Fixture::new();
    fs::write(copy.feature_home("copy-owner").join(".profile"), "copy\n").unwrap();
    copy.write_drop_in("drop-owner", ".profile.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&copy, ".profile", "copy-owner", "drop-owner");

    let snippets = Fixture::new();
    snippets.write_snippets(
        "snippet-owner",
        "snippets:\n  .bashrc:\n    - 'snippet managed'\n",
    );
    snippets.write_drop_in("drop-owner", ".bashrc.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&snippets, ".bashrc", "snippet-owner", "drop-owner");

    let drop_ancestor = Fixture::new();
    let home = drop_ancestor.feature_home("copy-owner");
    fs::create_dir_all(home.join(".config")).unwrap();
    fs::write(home.join(".config/tool"), "copy\n").unwrap();
    drop_ancestor.write_drop_in("drop-owner", ".config.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&drop_ancestor, ".config", "copy-owner", "drop-owner");

    let drop_descendant = Fixture::new();
    fs::write(
        drop_descendant.feature_home("copy-owner").join(".local"),
        "copy\n",
    )
    .unwrap();
    drop_descendant.write_drop_in("drop-owner", ".local/share.conf.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&drop_descendant, ".local", "copy-owner", "drop-owner");

    let nested_drop_ins = Fixture::new();
    let ancestor = nested_drop_ins.write_drop_in("ancestor", ".config.d", "10-base", b"ancestor\n");
    let descendant = nested_drop_ins.write_drop_in(
        "descendant",
        ".config/tool.conf.d",
        "20-tool",
        b"descendant\n",
    );
    let result = nested_drop_ins.lint();
    assert!(!result.status.success());
    let error = stderr(&result);
    assert!(error.contains(&ancestor.display().to_string()), "{error}");
    assert!(error.contains(&descendant.display().to_string()), "{error}");
}

#[test]
fn lint_rejects_ascii_case_aliases_involving_drop_ins() {
    let implied_parent = Fixture::new();
    let home = implied_parent.feature_home("copy-owner");
    fs::create_dir_all(home.join(".config")).unwrap();
    fs::write(home.join(".config/other"), "copy\n").unwrap();
    implied_parent.write_drop_in("drop-owner", ".Config/tool.conf.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&implied_parent, ".Config", "copy-owner", "drop-owner");

    let exact = Fixture::new();
    exact.write_snippets(
        "snippet-owner",
        "snippets:\n  .profile:\n    - 'snippet managed'\n",
    );
    exact.write_drop_in("drop-owner", ".PROFILE.d", "10-base", b"drop-in\n");
    assert_collision_mentions(&exact, ".PROFILE", "snippet-owner", "drop-owner");

    let two_drop_ins = Fixture::new();
    two_drop_ins.write_drop_in("alpha", "Tool.conf.d", "10-alpha", b"alpha\n");
    two_drop_ins.write_drop_in("zeta", "tool.CONF.d", "20-zeta", b"zeta\n");
    assert_collision_mentions(&two_drop_ins, "Tool.conf", "alpha", "zeta");
}

#[test]
fn lint_rejects_ascii_case_variants_of_dof_state_as_drop_in_targets() {
    for target in [".dof/config.yaml.d", ".DOF/config.yaml.d", ".DoF.d"] {
        let fixture = Fixture::new();
        fixture.write_drop_in("default", target, "10-base", b"managed\n");
        let result = fixture.lint();
        assert!(
            !result.status.success(),
            "unsafe target {target} passed lint"
        );
        assert!(stderr(&result).to_ascii_lowercase().contains(".dof"));
    }
}

fn assert_drop_in_lint_failure(fixture: &Fixture, expected_context: &str) {
    let result = fixture.lint();
    assert!(
        !result.status.success(),
        "{expected_context} unexpectedly passed lint"
    );
    assert!(
        stderr(&result).contains(expected_context),
        "expected diagnostic context {expected_context:?}, got: {}",
        stderr(&result)
    );
}

fn assert_collision_mentions(
    fixture: &Fixture,
    target: &str,
    first_feature: &str,
    second_feature: &str,
) {
    let result = fixture.lint();
    assert!(
        !result.status.success(),
        "collision at {target} unexpectedly passed"
    );
    let error = stderr(&result);
    assert!(
        error
            .to_ascii_lowercase()
            .contains(&target.to_ascii_lowercase()),
        "{error}"
    );
    assert!(error.contains(first_feature), "{error}");
    assert!(error.contains(second_feature), "{error}");
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

    fn drop_ins(&self, feature: &str) -> PathBuf {
        let root = self.feature(feature).join("drop-ins");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_drop_in(
        &self,
        feature: &str,
        target_directory: &str,
        fragment: &str,
        contents: &[u8],
    ) -> PathBuf {
        let directory = self.drop_ins(feature).join(target_directory);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(fragment);
        fs::write(&path, contents).unwrap();
        path
    }
}
