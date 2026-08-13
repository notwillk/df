use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Deserialize;
use tempfile::TempDir;

mod support;

use support::{binary, dof, output, stderr, stdout};

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Config {
    repo: RepoConfig,
    features: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct RepoConfig {
    url: String,
    branch: String,
    endpoint_fingerprint: Option<String>,
}

#[test]
fn help_and_version_describe_the_clone_command() {
    let help = output(Command::new(binary()).arg("clone").arg("--help"));
    assert!(help.status.success(), "{}", stderr(&help));
    let help_text = stdout(&help);
    assert!(help_text.contains("Usage: dof clone [OPTIONS] <REPOSITORY>"));
    assert!(help_text.contains("-b, --branch <BRANCH>"));
    assert!(help_text.contains("--force"));

    let version = output(Command::new(binary()).arg("--version"));
    assert!(version.status.success(), "{}", stderr(&version));
    assert_eq!(
        stdout(&version).trim(),
        concat!("dof ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn clone_requires_one_repository_and_rejects_a_destination() {
    let missing = output(Command::new(binary()).arg("clone"));
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("<REPOSITORY>"));

    let extra = output(
        Command::new(binary())
            .arg("clone")
            .arg("repository")
            .arg("destination"),
    );
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument 'destination'"));
}

#[test]
fn clones_main_and_writes_default_config() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        stdout(&result).trim(),
        fixture.workspace().display().to_string()
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(current_branch(&fixture.workspace()), "main");
    let config = read_config(&fixture.config());
    assert_eq!(config.repo.url, repository_url);
    assert_eq!(config.repo.branch, "main");
    let fingerprint = config
        .repo
        .endpoint_fingerprint
        .expect("fresh clones pin their effective repository endpoint");
    assert!(fingerprint.starts_with("sha256:"));
    assert_eq!(fingerprint.len(), "sha256:".len() + 64);
    assert!(
        fingerprint["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_eq!(config.features, BTreeMap::new());
}

#[test]
fn clones_a_custom_branch_with_short_or_long_flag() {
    for branch_flag in ["-b", "--branch"] {
        let fixture = Fixture::new();
        init_repository(&fixture.repository);
        create_branch(&fixture.repository, "laptop");
        let repository_url = fixture.repository_url();

        let result = output(
            dof(&fixture.home)
                .arg("clone")
                .arg(branch_flag)
                .arg("laptop")
                .arg(repository_url),
        );

        assert!(result.status.success(), "{}", stderr(&result));
        assert_eq!(current_branch(&fixture.workspace()), "laptop");
        assert_eq!(read_config(&fixture.config()).repo.branch, "laptop");
    }
}

#[test]
fn clone_fetches_unreachable_tags_without_persisting_a_tag_policy() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let tag = "unreachable-v1";
    let expected_tag = create_unreachable_tag(&fixture.repository, tag);

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(revision(&fixture.workspace(), tag), expected_tag);
    assert!(
        !fs::read_to_string(fixture.workspace().join(".git/config"))
            .unwrap()
            .contains("tagOpt")
    );
}

#[test]
fn branch_argument_is_treated_only_as_a_branch_name() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let object_id = stdout(&git(Some(&fixture.repository), ["rev-parse", "HEAD"]))
        .trim()
        .to_owned();

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .args(["--branch", &object_id])
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert_ne!(result.status.code(), Some(101), "gix must not panic");
    assert_no_managed_state(&fixture);
}

#[test]
fn same_repository_fast_forwards_without_recloning_or_rewriting_config() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();

    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    let config = format!(
        "repo:\n  url: {repository_url}\n  branch: main\nfeatures:\n  default: false\nfuture:\n  keep: true\n"
    );
    fs::write(fixture.config(), &config).unwrap();
    fs::write(fixture.workspace().join("local-only.txt"), "preserve\n").unwrap();

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");
    let expected_head = head(&fixture.repository);

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        stdout(&result).trim(),
        fixture.workspace().display().to_string()
    );
    assert_eq!(head(&fixture.workspace()), expected_head);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "updated\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("local-only.txt")).unwrap(),
        "preserve\n"
    );
    assert_eq!(fs::read_to_string(fixture.config()).unwrap(), config);
}

#[test]
fn same_repository_at_latest_or_locally_ahead_is_not_rewound() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    let current = head(&fixture.workspace());
    let current_config = fs::read(fixture.config()).unwrap();
    let at_latest = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(at_latest.status.success(), "{}", stderr(&at_latest));
    assert_eq!(head(&fixture.workspace()), current);
    assert_eq!(fs::read(fixture.config()).unwrap(), current_config);

    fs::write(fixture.workspace().join("tracked.txt"), "local ahead\n").unwrap();
    git(Some(&fixture.workspace()), ["add", "tracked.txt"]);
    commit(&fixture.workspace(), "local ahead");
    let local_head = head(&fixture.workspace());

    let ahead = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(ahead.status.success(), "{}", stderr(&ahead));
    assert_eq!(head(&fixture.workspace()), local_head);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "local ahead\n"
    );
    assert_eq!(fs::read(fixture.config()).unwrap(), current_config);
}

#[test]
fn divergent_repository_fails_without_changing_checkout_or_config() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    fs::write(fixture.workspace().join("tracked.txt"), "local\n").unwrap();
    git(Some(&fixture.workspace()), ["add", "tracked.txt"]);
    commit(&fixture.workspace(), "local");
    let local_head = head(&fixture.workspace());
    let config = fs::read(fixture.config()).unwrap();

    fs::write(fixture.repository.join("tracked.txt"), "remote\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "remote");

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("diverged"));
    assert_eq!(head(&fixture.workspace()), local_head);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "local\n"
    );
    assert_eq!(fs::read(fixture.config()).unwrap(), config);
}

#[test]
fn a_different_repository_or_branch_fails_without_changing_the_installation() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let original_head = head(&fixture.workspace());
    let original_config = fs::read(fixture.config()).unwrap();

    let other_repository = fixture.root.path().join("other-repository");
    init_repository(&other_repository);
    let different_repository = output(
        dof(&fixture.home)
            .arg("clone")
            .arg(file_url(&other_repository)),
    );
    assert!(!different_repository.status.success());
    assert!(stderr(&different_repository).contains("different repository or branch"));

    create_branch(&fixture.repository, "laptop");
    let different_branch = output(
        dof(&fixture.home)
            .args(["clone", "--branch", "laptop"])
            .arg(&repository_url),
    );
    assert!(!different_branch.status.success());
    assert!(stderr(&different_branch).contains("different repository or branch"));

    assert_eq!(head(&fixture.workspace()), original_head);
    assert_eq!(fs::read(fixture.config()).unwrap(), original_config);
}

#[test]
fn force_replaces_a_complete_installation_from_a_different_repository() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let first_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(first_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    fs::write(fixture.workspace().join("local-only.txt"), "remove\n").unwrap();

    let other_repository = fixture.root.path().join("other-repository");
    init_repository(&other_repository);
    fs::write(other_repository.join("tracked.txt"), "replacement\n").unwrap();
    git(Some(&other_repository), ["add", "tracked.txt"]);
    commit(&other_repository, "replacement");
    let other_url = file_url(&other_repository);

    let result = output(
        dof(&fixture.home)
            .args(["clone", "--force"])
            .arg(&other_url),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert!(!fixture.workspace().join("local-only.txt").exists());
    assert_eq!(head(&fixture.workspace()), head(&other_repository));
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "replacement\n"
    );
    assert_eq!(read_config(&fixture.config()).repo.url, other_url);
}

#[test]
fn failed_force_replacement_does_not_restore_old_managed_state() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let initial = output(
        dof(&fixture.home)
            .arg("clone")
            .arg(fixture.repository_url()),
    );
    assert!(initial.status.success(), "{}", stderr(&initial));
    fs::write(fixture.state_dir().join("unrelated.txt"), "keep\n").unwrap();

    let result = output(
        dof(&fixture.home)
            .args(["clone", "--force"])
            .arg(file_url(&fixture.root.path().join("missing"))),
    );

    assert!(!result.status.success());
    assert_no_managed_state(&fixture);
    assert_eq!(
        fs::read_to_string(fixture.state_dir().join("unrelated.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn incomplete_managed_state_fails_without_deleting_the_existing_half() {
    let workspace_only = Fixture::new();
    fs::create_dir_all(workspace_only.workspace()).unwrap();
    fs::write(workspace_only.workspace().join("keep.txt"), "workspace\n").unwrap();

    let result = output(dof(&workspace_only.home).arg("clone").arg("file:///unused"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("state is incomplete"));
    assert_eq!(
        fs::read_to_string(workspace_only.workspace().join("keep.txt")).unwrap(),
        "workspace\n"
    );
    assert!(!workspace_only.config().exists());

    let config_only = Fixture::new();
    fs::create_dir(config_only.state_dir()).unwrap();
    fs::write(config_only.config(), "keep: config\n").unwrap();

    let result = output(dof(&config_only.home).arg("clone").arg("file:///unused"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("state is incomplete"));
    assert_eq!(
        fs::read_to_string(config_only.config()).unwrap(),
        "keep: config\n"
    );
    assert!(!config_only.workspace().exists());
}

#[test]
fn update_refuses_an_ignored_file_that_would_be_overwritten() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    fs::write(fixture.repository.join(".gitignore"), "collision.txt\n").unwrap();
    git(Some(&fixture.repository), ["add", ".gitignore"]);
    commit(&fixture.repository, "ignore collision");
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    fs::write(fixture.workspace().join("collision.txt"), "local\n").unwrap();
    let original_head = head(&fixture.workspace());
    let original_config = fs::read(fixture.config()).unwrap();
    fs::write(fixture.repository.join("collision.txt"), "remote\n").unwrap();
    git(
        Some(&fixture.repository),
        ["add", "--force", "collision.txt"],
    );
    commit(&fixture.repository, "track collision");

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("fast-forward"));
    assert_eq!(head(&fixture.workspace()), original_head);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("collision.txt")).unwrap(),
        "local\n"
    );
    assert_eq!(fs::read(fixture.config()).unwrap(), original_config);
}

#[test]
fn update_preserves_nonconflicting_changes_and_refuses_tracked_conflicts() {
    let nonconflicting = Fixture::new();
    init_repository(&nonconflicting.repository);
    let repository_url = nonconflicting.repository_url();
    let initial = output(dof(&nonconflicting.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    fs::write(nonconflicting.workspace().join("local.txt"), "local\n").unwrap();
    fs::write(nonconflicting.repository.join("remote.txt"), "remote\n").unwrap();
    git(Some(&nonconflicting.repository), ["add", "remote.txt"]);
    commit(&nonconflicting.repository, "remote file");

    let result = output(dof(&nonconflicting.home).arg("clone").arg(&repository_url));
    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(nonconflicting.workspace().join("local.txt")).unwrap(),
        "local\n"
    );
    assert_eq!(
        fs::read_to_string(nonconflicting.workspace().join("remote.txt")).unwrap(),
        "remote\n"
    );

    let conflicting = Fixture::new();
    init_repository(&conflicting.repository);
    let repository_url = conflicting.repository_url();
    let initial = output(dof(&conflicting.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    fs::write(conflicting.workspace().join("tracked.txt"), "local\n").unwrap();
    let original_head = head(&conflicting.workspace());
    fs::write(conflicting.repository.join("tracked.txt"), "remote\n").unwrap();
    git(Some(&conflicting.repository), ["add", "tracked.txt"]);
    commit(&conflicting.repository, "remote conflict");

    let result = output(dof(&conflicting.home).arg("clone").arg(&repository_url));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("fast-forward"));
    assert_eq!(head(&conflicting.workspace()), original_head);
    assert_eq!(
        fs::read_to_string(conflicting.workspace().join("tracked.txt")).unwrap(),
        "local\n"
    );
}

#[test]
fn missing_git_during_update_preserves_the_checkout_and_config() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let original_head = head(&fixture.workspace());
    let original_config = fs::read(fixture.config()).unwrap();

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");
    let empty_path = fixture.root.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();

    let result = output(
        dof(&fixture.home)
            .env("PATH", empty_path)
            .arg("clone")
            .arg(&repository_url),
    );

    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to execute Git"));
    assert_eq!(head(&fixture.workspace()), original_head);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(fs::read(fixture.config()).unwrap(), original_config);
}

#[test]
fn relative_home_and_repository_can_be_updated() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let mut clone = Command::new(binary());
    clone
        .current_dir(fixture.root.path())
        .env("HOME", "home")
        .args(["clone", "repository"]);
    let initial = output(&mut clone);
    assert!(initial.status.success(), "{}", stderr(&initial));

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");

    let mut update = Command::new(binary());
    update
        .current_dir(fixture.root.path())
        .env("HOME", "home")
        .args(["clone", "repository"]);
    let result = output(&mut update);

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(head(&fixture.workspace()), head(&fixture.repository));
    assert_eq!(read_config(&fixture.config()).repo.url, "repository");
}

#[test]
fn update_uses_the_branch_configured_remote_even_when_it_is_not_origin() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    fs::write(
        fixture.home.join(".gitconfig"),
        "[clone]\n\tdefaultRemoteName = upstream\n",
    )
    .unwrap();
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    assert_eq!(
        stdout(&git(Some(&fixture.workspace()), ["remote"])).trim(),
        "upstream"
    );

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(result.status.success(), "{}", stderr(&result));
    let expected = head(&fixture.repository);
    assert_eq!(head(&fixture.workspace()), expected);
}

#[test]
fn update_rejects_workspace_metadata_for_another_remote_or_branch() {
    let remote_mismatch = Fixture::new();
    init_repository(&remote_mismatch.repository);
    let repository_url = remote_mismatch.repository_url();
    let initial = output(dof(&remote_mismatch.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let original_head = head(&remote_mismatch.workspace());
    let original_config = fs::read(remote_mismatch.config()).unwrap();
    let other_repository = remote_mismatch.root.path().join("other-repository");
    init_repository(&other_repository);
    let remote_name = stdout(&git(Some(&remote_mismatch.workspace()), ["remote"]))
        .trim()
        .to_owned();
    git(
        Some(&remote_mismatch.workspace()),
        [
            "remote",
            "set-url",
            &remote_name,
            &file_url(&other_repository),
        ],
    );

    let result = output(dof(&remote_mismatch.home).arg("clone").arg(&repository_url));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("remote does not match"));
    assert_eq!(head(&remote_mismatch.workspace()), original_head);
    assert_eq!(fs::read(remote_mismatch.config()).unwrap(), original_config);

    let branch_mismatch = Fixture::new();
    init_repository(&branch_mismatch.repository);
    let repository_url = branch_mismatch.repository_url();
    let initial = output(dof(&branch_mismatch.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    git(
        Some(&branch_mismatch.workspace()),
        ["switch", "-c", "local"],
    );

    let result = output(dof(&branch_mismatch.home).arg("clone").arg(&repository_url));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("not checked out on configured branch"));
    assert_eq!(current_branch(&branch_mismatch.workspace()), "local");
}

#[test]
fn update_rejects_a_tracking_ref_that_could_overwrite_a_local_branch() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    git(Some(&fixture.workspace()), ["switch", "-c", "victim"]);
    fs::write(fixture.workspace().join("victim.txt"), "preserve\n").unwrap();
    git(Some(&fixture.workspace()), ["add", "victim.txt"]);
    commit(&fixture.workspace(), "victim");
    let victim_head = head(&fixture.workspace());
    git(Some(&fixture.workspace()), ["switch", "main"]);
    let main_head = head(&fixture.workspace());
    let remote_name = stdout(&git(Some(&fixture.workspace()), ["remote"]))
        .trim()
        .to_owned();
    git(
        Some(&fixture.workspace()),
        [
            "config",
            "--replace-all",
            &format!("remote.{remote_name}.fetch"),
            "+refs/heads/main:refs/heads/victim",
        ],
    );

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("unsafe remote-tracking reference"));
    assert_eq!(head(&fixture.workspace()), main_head);
    assert_eq!(
        stdout(&git(
            Some(&fixture.workspace()),
            ["rev-parse", "refs/heads/victim"],
        ))
        .trim(),
        victim_head
    );
}

#[test]
fn update_uses_the_validated_url_not_a_global_remote_override() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let source_head = head(&fixture.repository);

    let other_repository = fixture.root.path().join("other-repository");
    git(
        None,
        ["clone", path(&fixture.repository), path(&other_repository)],
    );
    fs::write(other_repository.join("tracked.txt"), "wrong source\n").unwrap();
    git(Some(&other_repository), ["add", "tracked.txt"]);
    commit(&other_repository, "wrong source");
    let other_url = file_url(&other_repository);
    fs::write(
        fixture.home.join(".gitconfig"),
        format!("[remote \"origin\"]\n\turl = {other_url}\n"),
    )
    .unwrap();

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(head(&fixture.workspace()), source_head);
    assert_ne!(head(&fixture.workspace()), head(&other_repository));
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "main\n"
    );
}

#[test]
fn stable_preexisting_git_url_rewrite_can_clone_and_update() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let logical_url = "file:///dof-tests/logical-dotfiles";
    fs::write(
        fixture.home.join(".gitconfig"),
        format!(
            r#"[url "{repository_url}"]
    insteadOf = {logical_url}
"#,
        ),
    )
    .unwrap();

    let initial = output(dof(&fixture.home).arg("clone").arg(logical_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let config = read_config(&fixture.config());
    assert_eq!(config.repo.url, logical_url);
    assert!(config.repo.endpoint_fingerprint.is_some());
    assert!(
        !fs::read_to_string(fixture.workspace().join(".git/config"))
            .unwrap()
            .contains("tagOpt")
    );

    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");

    let update = output(dof(&fixture.home).arg("clone").arg(logical_url));

    assert!(update.status.success(), "{}", stderr(&update));
    assert_eq!(head(&fixture.workspace()), head(&fixture.repository));
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "updated\n"
    );
}

#[test]
fn update_rejects_a_git_url_rewrite_to_another_repository() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));
    let original_head = head(&fixture.workspace());
    let original_config = fs::read(fixture.config()).unwrap();

    let other_repository = fixture.root.path().join("other-repository");
    git(
        None,
        ["clone", path(&fixture.repository), path(&other_repository)],
    );
    fs::write(other_repository.join("tracked.txt"), "wrong source\n").unwrap();
    git(Some(&other_repository), ["add", "tracked.txt"]);
    commit(&other_repository, "wrong source");
    let other_url = file_url(&other_repository);
    fs::write(
        fixture.home.join(".gitconfig"),
        format!(
            r#"[url "{other_url}"]
    insteadOf = {repository_url}
"#,
        ),
    )
    .unwrap();

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("resolves to a different endpoint"));
    assert_eq!(head(&fixture.workspace()), original_head);
    assert_eq!(fs::read(fixture.config()).unwrap(), original_config);
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("tracked.txt")).unwrap(),
        "main\n"
    );
}

#[test]
fn update_does_not_follow_a_fetch_head_symlink() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    let initial = output(dof(&fixture.home).arg("clone").arg(&repository_url));
    assert!(initial.status.success(), "{}", stderr(&initial));

    let external = fixture.root.path().join("external.txt");
    fs::write(&external, "preserve\n").unwrap();
    let fetch_head = fixture.workspace().join(".git/FETCH_HEAD");
    if fs::symlink_metadata(&fetch_head).is_ok() {
        fs::remove_file(&fetch_head).unwrap();
    }
    symlink(&external, &fetch_head).unwrap();
    fs::write(fixture.repository.join("tracked.txt"), "updated\n").unwrap();
    git(Some(&fixture.repository), ["add", "tracked.txt"]);
    commit(&fixture.repository, "update");

    let result = output(dof(&fixture.home).arg("clone").arg(&repository_url));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(fs::read_to_string(&external).unwrap(), "preserve\n");
    assert!(
        fs::symlink_metadata(fetch_head)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(head(&fixture.workspace()), head(&fixture.repository));
}

#[test]
fn existing_state_fails_without_changing_either_path() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    fs::create_dir_all(fixture.workspace()).unwrap();
    fs::write(fixture.workspace().join("keep.txt"), "keep\n").unwrap();
    fs::write(fixture.config(), "keep: true\n").unwrap();

    let result = output(dof(&fixture.home).arg("clone").arg(repository_url));

    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(fixture.workspace().join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.config()).unwrap(),
        "keep: true\n"
    );
}

#[test]
fn force_deletes_existing_managed_state_before_cloning() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    fs::create_dir_all(fixture.workspace()).unwrap();
    fs::write(fixture.workspace().join("old.txt"), "old\n").unwrap();
    fs::write(fixture.config(), "old: true\n").unwrap();
    fs::write(fixture.state_dir().join("unrelated.txt"), "keep\n").unwrap();

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .arg("--force")
            .arg(repository_url),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert!(!fixture.workspace().join("old.txt").exists());
    assert!(fixture.workspace().join(".git").is_dir());
    assert_eq!(read_config(&fixture.config()).repo.branch, "main");
    assert_eq!(
        fs::read_to_string(fixture.state_dir().join("unrelated.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn invalid_repository_or_branch_leaves_no_managed_state() {
    let invalid_repository = Fixture::new();
    let result = output(
        dof(&invalid_repository.home)
            .arg("clone")
            .arg(file_url(&invalid_repository.root.path().join("missing"))),
    );
    assert!(!result.status.success());
    assert_no_managed_state(&invalid_repository);

    let missing_branch = Fixture::new();
    init_repository(&missing_branch.repository);
    let result = output(
        dof(&missing_branch.home)
            .arg("clone")
            .args(["--branch", "missing"])
            .arg(missing_branch.repository_url()),
    );
    assert!(!result.status.success());
    assert_no_managed_state(&missing_branch);
}

#[test]
fn missing_or_empty_home_fails_clearly() {
    let missing = output(
        Command::new(binary())
            .env_remove("HOME")
            .args(["clone", "file:///missing"]),
    );
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("HOME is not set or is empty"));

    let empty = output(
        Command::new(binary())
            .env("HOME", "")
            .args(["clone", "file:///missing"]),
    );
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("HOME is not set or is empty"));
}

#[test]
fn missing_git_upload_pack_fails_and_leaves_no_managed_state() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let empty_path = fixture.root.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();

    let result = output(
        dof(&fixture.home)
            .env("PATH", empty_path)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to fetch repository"));
    assert_no_managed_state(&fixture);
}

#[test]
fn command_scoped_git_config_does_not_redirect_a_fresh_clone() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let logical_url = "file:///dof-tests/command-scoped-logical";
    let result = output(
        dof(&fixture.home)
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.{}.insteadOf", fixture.repository_url()),
            )
            .env("GIT_CONFIG_VALUE_0", logical_url)
            .arg("clone")
            .arg(logical_url),
    );

    assert!(!result.status.success());
    assert_no_managed_state(&fixture);
}

#[test]
fn partial_clone_is_removed_when_git_upload_pack_fails() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let fake_bin = fixture.root.path().join("fake-bin");
    write_fake_upload_pack(
        &fake_bin,
        r#"#!/bin/sh
exit 42
"#,
    );

    let result = output(
        dof(&fixture.home)
            .env("PATH", fake_bin)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert_no_managed_state(&fixture);
}

#[test]
fn config_path_created_during_clone_is_preserved_and_workspace_is_removed() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let fake_bin = fixture.root.path().join("fake-bin");
    let upload_pack = git_upload_pack();
    write_fake_upload_pack(
        &fake_bin,
        &format!(
            r#"#!/bin/sh
/bin/mkdir "$HOME/.dof/config.yaml"
exec '{}' "$@"
"#,
            upload_pack.display()
        ),
    );

    let result = output(
        dof(&fixture.home)
            .env("PATH", fake_bin)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to create"));
    assert!(!fixture.workspace().exists());
    assert!(fixture.config().is_dir());
}

#[test]
fn symlinked_state_root_is_rejected_without_touching_its_target() {
    let fixture = Fixture::new();
    let external = fixture.root.path().join("external-state");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("keep.txt"), "keep\n").unwrap();
    symlink(&external, fixture.state_dir()).unwrap();

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real directory"));
    assert_eq!(
        fs::read_to_string(external.join("keep.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn dangling_managed_symlinks_count_as_existing() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.state_dir()).unwrap();
    symlink(
        fixture.root.path().join("missing-workspace"),
        fixture.workspace(),
    )
    .unwrap();
    symlink(fixture.root.path().join("missing-config"), fixture.config()).unwrap();

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .arg(fixture.repository_url()),
    );

    assert!(!result.status.success());
    assert!(
        fs::symlink_metadata(fixture.workspace())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(fixture.config())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn force_replaces_managed_symlinks_without_following_them() {
    let fixture = Fixture::new();
    init_repository(&fixture.repository);
    let repository_url = fixture.repository_url();
    fs::create_dir(fixture.state_dir()).unwrap();

    let external_workspace = fixture.root.path().join("external-workspace");
    fs::create_dir(&external_workspace).unwrap();
    fs::write(external_workspace.join("keep.txt"), "keep workspace\n").unwrap();
    let external_config = fixture.root.path().join("external-config.yaml");
    fs::write(&external_config, "keep config\n").unwrap();
    symlink(&external_workspace, fixture.workspace()).unwrap();
    symlink(&external_config, fixture.config()).unwrap();

    let result = output(
        dof(&fixture.home)
            .arg("clone")
            .arg("--force")
            .arg(repository_url),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(external_workspace.join("keep.txt")).unwrap(),
        "keep workspace\n"
    );
    assert_eq!(
        fs::read_to_string(external_config).unwrap(),
        "keep config\n"
    );
    assert!(fixture.workspace().is_dir());
    assert!(
        !fs::symlink_metadata(fixture.workspace())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !fs::symlink_metadata(fixture.config())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

struct Fixture {
    root: TempDir,
    home: PathBuf,
    repository: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let repository = root.path().join("repository");
        fs::create_dir(&home).unwrap();

        Self {
            root,
            home,
            repository,
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.home.join(".dof")
    }

    fn workspace(&self) -> PathBuf {
        self.state_dir().join("workspace")
    }

    fn config(&self) -> PathBuf {
        self.state_dir().join("config.yaml")
    }

    fn repository_url(&self) -> String {
        file_url(&self.repository)
    }
}

fn init_repository(repository: &Path) {
    git(None, ["init", "--initial-branch", "main", path(repository)]);
    fs::write(repository.join("tracked.txt"), "main\n").unwrap();
    git(Some(repository), ["add", "."]);
    commit(repository, "initial");
}

fn create_branch(repository: &Path, branch: &str) {
    git(Some(repository), ["switch", "-c", branch]);
    fs::write(repository.join("tracked.txt"), format!("{branch}\n")).unwrap();
    git(Some(repository), ["add", "."]);
    commit(repository, branch);
}

fn create_unreachable_tag(repository: &Path, tag: &str) -> String {
    git(Some(repository), ["switch", "--orphan", "tag-source"]);
    fs::write(repository.join("tag-only.txt"), "tag only\n").unwrap();
    git(Some(repository), ["add", "--all"]);
    commit(repository, "tag only");
    git(Some(repository), ["tag", tag]);
    let tagged_commit = head(repository);
    git(Some(repository), ["switch", "main"]);
    git(
        Some(repository),
        ["branch", "--delete", "--force", "tag-source"],
    );
    tagged_commit
}

fn commit(repository: &Path, message: &str) {
    git(
        Some(repository),
        [
            "-c",
            "user.name=dof tests",
            "-c",
            "user.email=dof@example.invalid",
            "commit",
            "-m",
            message,
        ],
    );
}

fn git<'a>(repository: Option<&Path>, arguments: impl IntoIterator<Item = &'a str>) -> Output {
    let mut command = Command::new("git");
    if let Some(repository) = repository {
        command.arg("-C").arg(repository);
    }
    command.args(arguments);
    let result = output(&mut command);
    assert!(result.status.success(), "git failed: {}", stderr(&result));
    result
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn head(repository: &Path) -> String {
    stdout(&git(Some(repository), ["rev-parse", "HEAD"]))
        .trim()
        .to_owned()
}

fn revision(repository: &Path, revision: &str) -> String {
    stdout(&git(Some(repository), ["rev-parse", revision]))
        .trim()
        .to_owned()
}

fn current_branch(workspace: &Path) -> String {
    let head = fs::read_to_string(workspace.join(".git/HEAD")).unwrap();
    head.strip_prefix("ref: refs/heads/")
        .expect("HEAD should point to a local branch")
        .trim()
        .to_owned()
}

fn read_config(path: &Path) -> Config {
    serde_yaml_ng::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_no_managed_state(fixture: &Fixture) {
    assert!(fs::symlink_metadata(fixture.workspace()).is_err());
    assert!(fs::symlink_metadata(fixture.config()).is_err());
}

fn file_url(path: &Path) -> String {
    assert!(path.is_absolute());
    format!("file://{}", path.display())
}

fn git_upload_pack() -> PathBuf {
    let result = git(None, ["--exec-path"]);
    PathBuf::from(stdout(&result).trim()).join("git-upload-pack")
}

fn write_fake_upload_pack(directory: &Path, script: &str) {
    fs::create_dir(directory).unwrap();
    let upload_pack = directory.join("git-upload-pack");
    fs::write(&upload_pack, script).unwrap();
    let mut permissions = fs::metadata(&upload_pack).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(upload_pack, permissions).unwrap();
}
