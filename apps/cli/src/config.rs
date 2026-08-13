use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;
use tempfile::NamedTempFile;

use crate::state::DofPaths;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Config {
    pub(crate) repo: RepoConfig,
    #[serde(default)]
    features: BTreeMap<String, bool>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

impl Config {
    pub(crate) fn new(repo: RepoConfig) -> Self {
        Self {
            repo,
            features: BTreeMap::new(),
            extensions: BTreeMap::new(),
        }
    }

    pub(crate) fn feature_enabled(&self, name: &str) -> bool {
        self.features.get(name).copied().unwrap_or(true)
    }

    pub(crate) fn set_feature_enabled(&mut self, name: &str, enabled: bool) {
        self.features.insert(name.to_owned(), enabled);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RepoConfig {
    pub(crate) url: String,
    pub(crate) branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) endpoint_fingerprint: Option<String>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

impl RepoConfig {
    pub(crate) fn new(url: String, branch: String, endpoint_fingerprint: String) -> Self {
        Self {
            url,
            branch,
            endpoint_fingerprint: Some(endpoint_fingerprint),
            extensions: BTreeMap::new(),
        }
    }
}

/// Durable access to the configuration stored under one dof state directory.
#[derive(Clone, Debug)]
pub(crate) struct ConfigStore {
    paths: DofPaths,
}

impl ConfigStore {
    pub(crate) fn new(paths: &DofPaths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }

    pub(crate) fn create(&self, config: &Config) -> Result<()> {
        self.paths.require_state_dir()?;
        let contents = serialize(config)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.paths.config())
            .with_context(|| format!("failed to create {}", self.paths.config().display()))?;
        let created_metadata = file.metadata().with_context(|| {
            format!(
                "failed to inspect new config {}",
                self.paths.config().display()
            )
        })?;

        let write_result = file
            .write_all(contents.as_bytes())
            .with_context(|| format!("failed to write {}", self.paths.config().display()))
            .and_then(|()| {
                file.sync_all()
                    .with_context(|| format!("failed to sync {}", self.paths.config().display()))
            });
        drop(file);
        if let Err(error) = write_result {
            return Err(cleanup_created_config(
                error,
                self.paths.config(),
                &created_metadata,
            ));
        }

        if let Err(error) = sync_directory(self.paths.state_dir(), "config directory") {
            return Err(cleanup_created_config(
                error,
                self.paths.config(),
                &created_metadata,
            ));
        }
        Ok(())
    }

    pub(crate) fn read(&self) -> Result<Config> {
        self.paths.require_state_dir()?;
        Ok(read_snapshot(self.paths.config())?.config)
    }

    /// Serialize updates through a lock on the real state directory, then
    /// replace the file atomically. The callback runs while the lock is held
    /// and may perform related workspace validation.
    pub(crate) fn update(&self, update: impl FnOnce(&mut Config) -> Result<()>) -> Result<()> {
        self.paths.require_state_dir()?;
        let lock = File::open(self.paths.state_dir()).with_context(|| {
            format!(
                "failed to open dof state directory {} for locking",
                self.paths.state_dir().display()
            )
        })?;
        lock.lock().with_context(|| {
            format!(
                "failed to lock dof state directory {}",
                self.paths.state_dir().display()
            )
        })?;

        let mut snapshot = read_snapshot(self.paths.config())?;
        update(&mut snapshot.config)?;
        let updated = serialize(&snapshot.config)?;
        self.replace_atomically(updated.as_bytes(), &snapshot)
    }

    fn replace_atomically(&self, contents: &[u8], original: &Snapshot) -> Result<()> {
        self.paths.require_state_dir()?;
        let mode = original.metadata.permissions().mode() & 0o7777;
        let staged = stage(self.paths.state_dir(), self.paths.config(), contents, mode)?;

        let current_metadata = fs::symlink_metadata(self.paths.config()).with_context(|| {
            format!(
                "failed to re-inspect dof config {}",
                self.paths.config().display()
            )
        })?;
        if !same_file_state(&current_metadata, &original.metadata)
            || fs::read(self.paths.config()).with_context(|| {
                format!(
                    "failed to re-read dof config {}",
                    self.paths.config().display()
                )
            })? != original.contents
        {
            bail!(
                "dof config {} changed while it was being updated; retry the command",
                self.paths.config().display()
            );
        }

        staged
            .persist(self.paths.config())
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "failed to replace dof config {}",
                    self.paths.config().display()
                )
            })?;

        sync_directory(self.paths.state_dir(), "config directory").map_err(|error| {
            anyhow!(
                "config was updated, but failed to sync directory {}: {error:#}",
                self.paths.state_dir().display()
            )
        })
    }
}

struct Snapshot {
    config: Config,
    metadata: fs::Metadata,
    contents: Vec<u8>,
}

fn read_snapshot(path: &Path) -> Result<Snapshot> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(error)
                .with_context(|| format!("failed to read dof config {}", path.display()));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect dof config {}", path.display()));
        }
    };
    if !metadata.file_type().is_file() {
        bail!("dof config {} is not a real file", path.display());
    }

    let contents =
        fs::read(path).with_context(|| format!("failed to read dof config {}", path.display()))?;
    let config = serde_yaml_ng::from_slice(&contents)
        .with_context(|| format!("failed to parse dof config {}", path.display()))?;
    Ok(Snapshot {
        config,
        metadata,
        contents,
    })
}

fn serialize(config: &Config) -> Result<String> {
    serde_yaml_ng::to_string(config).context("failed to serialize dof config")
}

fn stage(parent: &Path, path: &Path, contents: &[u8], mode: u32) -> Result<NamedTempFile> {
    let mut staged = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create staged config in directory {}",
            parent.display()
        )
    })?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| {
            format!(
                "failed to set permissions for staged config {}",
                path.display()
            )
        })?;
    staged
        .write_all(contents)
        .with_context(|| format!("failed to write staged config {}", path.display()))?;
    staged
        .as_file_mut()
        .sync_all()
        .with_context(|| format!("failed to sync staged config {}", path.display()))?;
    Ok(staged)
}

fn same_file_state(current: &fs::Metadata, original: &fs::Metadata) -> bool {
    current.file_type().is_file()
        && current.dev() == original.dev()
        && current.ino() == original.ino()
        && current.mode() == original.mode()
}

fn sync_directory(path: &Path, label: &str) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open {label} {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {label} {}", path.display()))
}

fn cleanup_created_config(
    error: anyhow::Error,
    path: &Path,
    created_metadata: &fs::Metadata,
) -> anyhow::Error {
    let cleanup = (|| -> Result<()> {
        let current = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect partial config {}", path.display()))?;
        if !same_file_state(&current, created_metadata) {
            bail!("partial config {} changed before cleanup", path.display());
        }
        fs::remove_file(path)
            .with_context(|| format!("failed to remove partial config {}", path.display()))
    })();
    match cleanup {
        Ok(()) => error,
        Err(cleanup_error) => {
            anyhow!("{error:#}; failed to clean up partial config: {cleanup_error:#}")
        }
    }
}
