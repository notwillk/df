use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

use crate::workspace::HomePath;

/// The observed identity of a managed destination during apply preflight.
///
/// Identity is intentionally stronger than just file type: an apply refuses to
/// replace a path whose inode, mode, size, or timestamps changed after planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedState {
    Missing,
    Regular(FileIdentity),
    Symlink(FileIdentity),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    pub(crate) fn full_mode(&self) -> u32 {
        self.mode & 0o7777
    }

    pub(crate) fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        *self == Self::from_metadata(metadata)
    }
}

impl ExpectedState {
    fn matches_open_regular(&self, metadata: &fs::Metadata) -> bool {
        matches!(self, Self::Regular(identity) if identity.matches_metadata(metadata))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CopySource {
    path: PathBuf,
    identity: FileIdentity,
}

impl CopySource {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn mode(&self) -> u32 {
        self.identity.mode & 0o777
    }

    pub(crate) fn length(&self) -> u64 {
        self.identity.length
    }
}

pub(crate) enum ObservedTarget {
    Missing,
    Regular {
        expected: ExpectedState,
        mode: u32,
        length: u64,
    },
    Symlink {
        expected: ExpectedState,
    },
    Directory,
    Unsupported,
}

pub(crate) enum StageSource<'a> {
    Copy(&'a CopySource),
    Bytes(&'a [u8]),
}

/// Concrete, path-based access to the real HOME tree.
///
/// `HomePath` proves lexical path safety. This component owns the remaining
/// runtime checks: no destination ancestor may be a symlink, and observations
/// made during planning are revalidated while changes are executed.
pub(crate) struct HomeFs {
    root: PathBuf,
}

impl HomeFs {
    pub(crate) fn new(root: &Path) -> Result<Self> {
        let metadata = fs::metadata(root)
            .with_context(|| format!("failed to inspect home directory {}", root.display()))?;
        if !metadata.is_dir() {
            bail!("home directory {} is not a real directory", root.display());
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub(crate) fn path(&self, target: &HomePath) -> PathBuf {
        self.root.join(target.as_path())
    }

    pub(crate) fn inspect_source(&self, path: PathBuf) -> Result<CopySource> {
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect source file {}", path.display()))?;
        if !metadata.file_type().is_file() {
            bail!(
                "source file {} changed after workspace validation",
                path.display()
            );
        }
        Ok(CopySource {
            path,
            identity: FileIdentity::from_metadata(&metadata),
        })
    }

    pub(crate) fn observe(&self, target: &HomePath) -> Result<ObservedTarget> {
        let destination = self.path(target);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let identity = FileIdentity::from_metadata(&metadata);
                Ok(ObservedTarget::Regular {
                    mode: identity.full_mode(),
                    length: identity.length,
                    expected: ExpectedState::Regular(identity),
                })
            }
            Ok(metadata) if metadata.file_type().is_symlink() => Ok(ObservedTarget::Symlink {
                expected: ExpectedState::Symlink(FileIdentity::from_metadata(&metadata)),
            }),
            Ok(metadata) if metadata.file_type().is_dir() => Ok(ObservedTarget::Directory),
            Ok(_) => Ok(ObservedTarget::Unsupported),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ObservedTarget::Missing),
            Err(error) => Err(error).with_context(|| {
                format!("failed to inspect destination {}", destination.display())
            }),
        }
    }

    pub(crate) fn read_text(&self, target: &HomePath, expected: &ExpectedState) -> Result<String> {
        let destination = self.path(target);
        let mut file = File::open(&destination).with_context(|| {
            format!(
                "failed to open snippet target {} as UTF-8 text",
                destination.display()
            )
        })?;
        verify_open_destination(&file, &destination, expected)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).with_context(|| {
            format!(
                "failed to read snippet target {} as UTF-8 text",
                destination.display()
            )
        })?;
        verify_open_destination(&file, &destination, expected)?;
        self.revalidate(target, expected)?;
        Ok(contents)
    }

    pub(crate) fn contents_equal(
        &self,
        source: &CopySource,
        target: &HomePath,
        expected: &ExpectedState,
    ) -> Result<bool> {
        let destination = self.path(target);
        let mut source_file = File::open(source.path())
            .with_context(|| format!("failed to open source file {}", source.path().display()))?;
        verify_open_source(&source_file, source)?;
        let mut destination_file = File::open(&destination).with_context(|| {
            format!("failed to open destination file {}", destination.display())
        })?;
        verify_open_destination(&destination_file, &destination, expected)?;
        let mut source_buffer = [0_u8; 16 * 1024];
        let mut destination_buffer = [0_u8; 16 * 1024];
        let mut remaining = source.length();
        let mut equal = true;

        while remaining > 0 {
            let chunk_size = usize::try_from(remaining.min(source_buffer.len() as u64))
                .expect("bounded chunk size fits usize");
            source_file
                .read_exact(&mut source_buffer[..chunk_size])
                .with_context(|| {
                    format!("failed to read source file {}", source.path().display())
                })?;
            destination_file
                .read_exact(&mut destination_buffer[..chunk_size])
                .with_context(|| {
                    format!("failed to read destination file {}", destination.display())
                })?;
            if source_buffer[..chunk_size] != destination_buffer[..chunk_size] {
                equal = false;
                break;
            }
            remaining -= chunk_size as u64;
        }

        verify_open_source(&source_file, source)?;
        verify_open_destination(&destination_file, &destination, expected)?;
        self.revalidate(target, expected)?;
        Ok(equal)
    }

    /// Compares generated bytes with a regular destination while preserving
    /// the destination identity guarantees used during apply planning.
    pub(crate) fn contents_equal_bytes(
        &self,
        contents: &[u8],
        target: &HomePath,
        expected: &ExpectedState,
    ) -> Result<bool> {
        let destination = self.path(target);
        let mut destination_file = File::open(&destination).with_context(|| {
            format!("failed to open destination file {}", destination.display())
        })?;
        verify_open_destination(&destination_file, &destination, expected)?;

        let mut existing = Vec::with_capacity(contents.len());
        destination_file
            .read_to_end(&mut existing)
            .with_context(|| {
                format!("failed to read destination file {}", destination.display())
            })?;

        verify_open_destination(&destination_file, &destination, expected)?;
        self.revalidate(target, expected)?;
        Ok(existing == contents)
    }

    /// Returns true when the final directory does not exist.
    pub(crate) fn preflight_directory(&self, target: &HomePath) -> Result<bool> {
        let mut current = self.root.clone();
        let component_count = target.as_path().components().count();
        let mut leaf_missing = false;

        for (index, component) in target.as_path().components().enumerate() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => bail!(
                    "destination directory {} is blocked by a non-directory path {}",
                    self.path(target).display(),
                    current.display()
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if index + 1 == component_count {
                        leaf_missing = true;
                    }
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to inspect destination path {}", current.display())
                    });
                }
            }
        }

        Ok(leaf_missing)
    }

    pub(crate) fn preflight_file_parent(&self, target: &HomePath) -> Result<()> {
        for parent in target.parents() {
            self.preflight_directory(&parent)?;
        }
        Ok(())
    }

    pub(crate) fn create_directory(&self, target: &HomePath) -> Result<()> {
        let destination = self.path(target);
        fs::create_dir(&destination).with_context(|| {
            format!(
                "failed to create destination directory {}",
                destination.display()
            )
        })
    }

    pub(crate) fn stage(
        &self,
        target: &HomePath,
        source: StageSource<'_>,
        mode: u32,
    ) -> Result<NamedTempFile> {
        self.preflight_file_parent(target)?;
        let destination = self.path(target);
        let parent = destination.parent().with_context(|| {
            format!(
                "destination file {} does not have a parent directory",
                destination.display()
            )
        })?;
        let mut staged = NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "failed to create staged file in destination directory {}",
                parent.display()
            )
        })?;

        match source {
            StageSource::Copy(source) => {
                let mut source_file = File::open(source.path()).with_context(|| {
                    format!("failed to open source file {}", source.path().display())
                })?;
                verify_open_source(&source_file, source)?;
                io::copy(&mut source_file, staged.as_file_mut()).with_context(|| {
                    format!("failed to stage source file {}", source.path().display())
                })?;
                verify_open_source(&source_file, source)?;
            }
            StageSource::Bytes(contents) => {
                staged.write_all(contents).with_context(|| {
                    format!("failed to stage managed file {}", destination.display())
                })?;
            }
        }

        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| {
                format!(
                    "failed to set permissions for staged file {}",
                    destination.display()
                )
            })?;
        staged
            .as_file_mut()
            .sync_all()
            .with_context(|| format!("failed to sync staged file for {}", destination.display()))?;
        Ok(staged)
    }

    pub(crate) fn revalidate(&self, target: &HomePath, expected: &ExpectedState) -> Result<()> {
        let destination = self.path(target);
        let matches = match (expected, fs::symlink_metadata(&destination)) {
            (ExpectedState::Missing, Err(error)) if error.kind() == io::ErrorKind::NotFound => true,
            (ExpectedState::Regular(expected), Ok(metadata)) if metadata.file_type().is_file() => {
                FileIdentity::from_metadata(&metadata) == *expected
            }
            (ExpectedState::Symlink(expected), Ok(metadata))
                if metadata.file_type().is_symlink() =>
            {
                FileIdentity::from_metadata(&metadata) == *expected
            }
            (_, Err(error)) if error.kind() != io::ErrorKind::NotFound => {
                return Err(error).with_context(|| {
                    format!("failed to re-inspect destination {}", destination.display())
                });
            }
            _ => false,
        };

        if !matches {
            bail!(
                "destination {} changed after apply preflight; retry the command",
                destination.display()
            );
        }
        Ok(())
    }

    pub(crate) fn install(
        &self,
        target: &HomePath,
        staged: NamedTempFile,
        expected: &ExpectedState,
    ) -> Result<()> {
        self.preflight_file_parent(target)?;
        self.revalidate(target, expected)?;
        let destination = self.path(target);
        match expected {
            ExpectedState::Missing => staged
                .persist_noclobber(&destination)
                .map_err(|error| error.error)
                .map(|_| ())
                .with_context(|| format!("failed to install new file {}", destination.display())),
            ExpectedState::Regular(_) | ExpectedState::Symlink(_) => staged
                .persist(&destination)
                .map_err(|error| error.error)
                .map(|_| ())
                .with_context(|| format!("failed to replace file {}", destination.display())),
        }
    }
}

fn verify_open_source(file: &File, source: &CopySource) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect source file {}", source.path().display()))?;
    if !metadata.file_type().is_file() || FileIdentity::from_metadata(&metadata) != source.identity
    {
        bail!(
            "source file {} changed after apply preflight",
            source.path().display()
        );
    }
    Ok(())
}

fn verify_open_destination(
    file: &File,
    destination: &Path,
    expected: &ExpectedState,
) -> Result<()> {
    let metadata = file.metadata().with_context(|| {
        format!(
            "failed to inspect destination file {}",
            destination.display()
        )
    })?;
    if !metadata.file_type().is_file() || !expected.matches_open_regular(&metadata) {
        bail!(
            "destination {} changed after apply preflight; retry the command",
            destination.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_state_detects_a_same_shape_replacement() {
        let root = tempfile::tempdir().unwrap();
        let home = HomeFs::new(root.path()).unwrap();
        let target = HomePath::new("managed").unwrap();
        let destination = home.path(&target);
        fs::write(&destination, "same length").unwrap();
        let expected = match home.observe(&target).unwrap() {
            ObservedTarget::Regular { expected, .. } => expected,
            _ => panic!("expected regular file"),
        };

        fs::rename(&destination, root.path().join("original")).unwrap();
        fs::write(&destination, "same length").unwrap();

        let error = home.revalidate(&target, &expected).unwrap_err();
        assert!(format!("{error:#}").contains("changed after apply preflight"));
    }

    #[test]
    fn missing_expected_state_detects_a_new_destination() {
        let root = tempfile::tempdir().unwrap();
        let home = HomeFs::new(root.path()).unwrap();
        let target = HomePath::new("new-managed").unwrap();
        assert!(matches!(
            home.observe(&target).unwrap(),
            ObservedTarget::Missing
        ));

        fs::write(home.path(&target), "appeared").unwrap();

        assert!(home.revalidate(&target, &ExpectedState::Missing).is_err());
    }
}
