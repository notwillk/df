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
    assert_eq!(
        read_config(&fixture.config()),
        Config {
            repo: RepoConfig {
                url: repository_url,
                branch: "main".into(),
            },
            features: BTreeMap::new(),
        }
    );
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
