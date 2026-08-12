#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::de::DeserializeOwned;
use tempfile::TempDir;

const DEFAULT_CONFIG: &str = r#"repo:
  url: file:///dotfiles
  branch: main
features: {}
"#;

/// Isolated HOME and dof-managed paths shared by command integration tests.
///
/// `new` creates the real `.dof/workspace` hierarchy but deliberately leaves
/// config absent so tests can exercise missing and malformed configurations.
pub struct ManagedStateFixture {
    pub root: TempDir,
    pub home: PathBuf,
    pub state_dir: PathBuf,
    pub workspace: PathBuf,
    pub config: PathBuf,
}

impl ManagedStateFixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state_dir = home.join(".dof");
        let workspace = state_dir.join("workspace");
        let config = state_dir.join("config.yaml");
        fs::create_dir_all(&workspace).unwrap();
        Self {
            root,
            home,
            state_dir,
            workspace,
            config,
        }
    }

    pub fn with_default_config() -> Self {
        let fixture = Self::new();
        fixture.write_config(DEFAULT_CONFIG);
        fixture
    }

    pub fn write_config(&self, yaml: &str) {
        fs::write(&self.config, yaml).unwrap();
    }

    pub fn read_config<T: DeserializeOwned>(&self) -> T {
        serde_yaml_ng::from_str(&fs::read_to_string(&self.config).unwrap()).unwrap()
    }

    pub fn create_feature(&self, name: &str) {
        fs::create_dir(self.workspace.join(name)).unwrap();
    }

    pub fn feature(&self, name: &str) -> PathBuf {
        let path = self.workspace.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    pub fn feature_home(&self, name: &str) -> PathBuf {
        let home = self.feature(name).join("home");
        fs::create_dir_all(&home).unwrap();
        home
    }
}

impl Default for ManagedStateFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_dof")
}

pub fn dof(home: &Path) -> Command {
    let mut command = Command::new(binary());
    command.env("HOME", home);
    command
}

pub fn output(command: &mut Command) -> Output {
    command.output().unwrap()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn set_mode(path: &Path, mode: u32) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).unwrap();
}

pub fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

pub fn full_mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

pub fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    set_mode(path, 0o755);
}
