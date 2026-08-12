use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::home_fs::{ExpectedState, HomeFs};
use crate::workspace::HomePath;

/// Lazily creates and owns the single private backup snapshot for an apply.
pub(crate) struct BackupStore {
    root: PathBuf,
    snapshot: Option<PathBuf>,
}

impl BackupStore {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            snapshot: None,
        }
    }

    pub(crate) fn preflight(root: &Path) -> Result<()> {
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                if access_mode(&metadata) & 0o077 != 0 {
                    bail!(
                        "backup directory {} is not private; remove group and other permissions",
                        root.display()
                    );
                }
                Ok(())
            }
            Ok(_) => bail!("backup path {} is not a real directory", root.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to inspect backup path {}", root.display())),
        }
    }

    pub(crate) fn snapshot(&self) -> Option<&Path> {
        self.snapshot.as_deref()
    }

    pub(crate) fn back_up(
        &mut self,
        home: &HomeFs,
        target: &HomePath,
        expected: &ExpectedState,
    ) -> Result<()> {
        // The executor checks immediately before calling us. Check once more so
        // BackupStore never silently backs up a different kind of object.
        home.revalidate(target, expected)?;
        let source = home.path(target);
        match expected {
            ExpectedState::Regular(_) => self.back_up_regular(&source, target, expected),
            ExpectedState::Symlink(_) => self.back_up_symlink(&source, target, expected),
            ExpectedState::Missing => {
                bail!("cannot back up missing destination {}", source.display())
            }
        }
    }

    fn back_up_regular(
        &mut self,
        source: &Path,
        target: &HomePath,
        expected: &ExpectedState,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(source)
            .with_context(|| format!("failed to inspect file for backup {}", source.display()))?;
        if !metadata.file_type().is_file() {
            bail!(
                "backup source {} changed from a regular file",
                source.display()
            );
        }
        let mut source_file = File::open(source)
            .with_context(|| format!("failed to open file for backup {}", source.display()))?;
        // Opening a path may race with replacement; compare the opened inode to
        // the expected path identity before copying any bytes.
        let opened_metadata = source_file.metadata().with_context(|| {
            format!(
                "failed to inspect open file for backup {}",
                source.display()
            )
        })?;
        if let ExpectedState::Regular(identity) = expected
            && !identity.matches_metadata(&opened_metadata)
        {
            bail!(
                "backup source {} changed while it was opened",
                source.display()
            );
        }
        let backup_path = self.backup_path(target)?;
        let mut backup_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup_path)
            .with_context(|| format!("failed to create backup {}", backup_path.display()))?;

        let backup_result = (|| -> Result<()> {
            io::copy(&mut source_file, &mut backup_file)
                .with_context(|| format!("failed to copy backup {}", backup_path.display()))?;
            let copied_metadata = source_file.metadata().with_context(|| {
                format!(
                    "failed to re-inspect open file for backup {}",
                    source.display()
                )
            })?;
            if let ExpectedState::Regular(identity) = expected
                && !identity.matches_metadata(&copied_metadata)
            {
                bail!(
                    "backup source {} changed while it was copied",
                    source.display()
                );
            }
            backup_file
                .set_permissions(fs::Permissions::from_mode(
                    opened_metadata.permissions().mode() & 0o7777,
                ))
                .with_context(|| {
                    format!(
                        "failed to set permissions on backup {}",
                        backup_path.display()
                    )
                })?;
            backup_file
                .sync_all()
                .with_context(|| format!("failed to sync backup {}", backup_path.display()))?;
            Ok(())
        })();
        drop(backup_file);

        if let Err(error) = backup_result {
            if let Err(cleanup_error) = fs::remove_file(&backup_path) {
                return Err(anyhow!(
                    "{error:#}; failed to remove partial backup {}: {cleanup_error}",
                    backup_path.display()
                ));
            }
            return Err(error);
        }

        Ok(())
    }

    fn back_up_symlink(
        &mut self,
        source: &Path,
        target: &HomePath,
        expected: &ExpectedState,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(source).with_context(|| {
            format!("failed to inspect symlink for backup {}", source.display())
        })?;
        if !metadata.file_type().is_symlink() {
            bail!("backup source {} changed from a symlink", source.display());
        }
        let link_target = fs::read_link(source)
            .with_context(|| format!("failed to read symlink for backup {}", source.display()))?;
        // Detect a changed link target/inode after read_link and before creating
        // the backup. The executor will independently recheck before replace.
        let current = fs::symlink_metadata(source).with_context(|| {
            format!(
                "failed to re-inspect symlink for backup {}",
                source.display()
            )
        })?;
        let expected_matches = match expected {
            ExpectedState::Symlink(identity) => identity.matches_metadata(&current),
            _ => false,
        };
        if !current.file_type().is_symlink() || !expected_matches {
            bail!(
                "backup source {} changed while it was read",
                source.display()
            );
        }
        let backup_path = self.backup_path(target)?;
        symlink(&link_target, &backup_path)
            .with_context(|| format!("failed to create symlink backup {}", backup_path.display()))
    }

    fn backup_path(&mut self, target: &HomePath) -> Result<PathBuf> {
        let snapshot = self.ensure_snapshot()?;
        if let Some(parent) = target.as_path().parent()
            && !parent.as_os_str().is_empty()
        {
            ensure_private_relative_directories(&snapshot, parent)?;
        }
        Ok(snapshot.join(target.as_path()))
    }

    fn ensure_snapshot(&mut self) -> Result<PathBuf> {
        if let Some(snapshot) = &self.snapshot {
            return Ok(snapshot.clone());
        }

        ensure_private_directory(&self.root)?;
        let timestamp = jiff::Timestamp::now()
            .strftime("%Y%m%dT%H%M%S.%9fZ")
            .to_string();

        let mut suffix = 0_u64;
        loop {
            let name = if suffix == 0 {
                timestamp.clone()
            } else {
                format!("{timestamp}-{suffix}")
            };
            let candidate = self.root.join(name);
            match create_private_directory(&candidate) {
                Ok(()) => {
                    self.snapshot = Some(candidate.clone());
                    return Ok(candidate);
                }
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|error| error.kind() == io::ErrorKind::AlreadyExists) =>
                {
                    suffix += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            if access_mode(&metadata) & 0o077 != 0 {
                bail!(
                    "backup directory {} is not private; remove group and other permissions",
                    path.display()
                );
            }
            return Ok(());
        }
        Ok(_) => bail!("backup path {} is not a real directory", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect backup directory {}", path.display()));
        }
    }

    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).with_context(|| {
        format!(
            "failed to create private backup directory {}",
            path.display()
        )
    })?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o700)) {
        let _ = fs::remove_dir(path);
        return Err(error)
            .with_context(|| format!("failed to secure backup directory {}", path.display()));
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o700)) {
        let _ = fs::remove_dir(path);
        return Err(error)
            .with_context(|| format!("failed to secure backup directory {}", path.display()));
    }
    Ok(())
}

fn ensure_private_relative_directories(root: &Path, relative_path: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        current.push(component.as_os_str());
        ensure_private_directory(&current)?;
    }
    Ok(())
}

fn access_mode(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode() & 0o777
}
