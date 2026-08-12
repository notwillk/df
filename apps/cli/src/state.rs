use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The complete set of paths owned by dof for one HOME.
///
/// Constructing this value only resolves names. Commands opt into the state
/// checks they need so, for example, `clone` can create `.dof` while `apply`
/// requires it to exist already.
#[derive(Clone, Debug)]
pub(crate) struct DofPaths {
    home: PathBuf,
    state_dir: PathBuf,
    workspace: PathBuf,
    config: PathBuf,
    bin: PathBuf,
    backups: PathBuf,
}

impl DofPaths {
    pub(crate) fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .context("HOME is not set or is empty")?;
        Ok(Self::from_home(PathBuf::from(home)))
    }

    fn from_home(home: PathBuf) -> Self {
        let state_dir = home.join(".dof");
        Self {
            workspace: state_dir.join("workspace"),
            config: state_dir.join("config.yaml"),
            bin: state_dir.join("bin"),
            backups: state_dir.join("backups"),
            home,
            state_dir,
        }
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn config(&self) -> &Path {
        &self.config
    }

    pub(crate) fn bin(&self) -> &Path {
        &self.bin
    }

    pub(crate) fn backups(&self) -> &Path {
        &self.backups
    }

    pub(crate) fn ensure_state_dir(&self) -> Result<()> {
        match fs::symlink_metadata(&self.state_dir) {
            Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
            Ok(_) => bail!(
                "dof state path {} exists but is not a real directory",
                self.state_dir.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&self.state_dir) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        require_real_directory(&self.state_dir, "dof state path")
                    }
                    Err(error) => Err(error).with_context(|| {
                        format!(
                            "failed to create dof state directory {}",
                            self.state_dir.display()
                        )
                    }),
                }
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to inspect dof state path {}",
                    self.state_dir.display()
                )
            }),
        }
    }

    pub(crate) fn require_state_dir(&self) -> Result<()> {
        require_real_directory(&self.state_dir, "dof state directory")
    }

    pub(crate) fn require_state_dir_if_present(&self) -> Result<()> {
        require_real_directory_if_present(&self.state_dir, "dof state directory")
    }

    pub(crate) fn require_bin_dir(&self) -> Result<()> {
        self.require_state_dir_if_present()?;
        require_real_directory(&self.bin, "dof bin directory")
    }
}

fn require_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("{label} {} is not a real directory", path.display());
    }
    Ok(())
}

fn require_real_directory_if_present(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => bail!("{label} {} is not a real directory", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {label} {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn derives_every_managed_path_from_home() {
        let paths = DofPaths::from_home(PathBuf::from("relative-home"));
        assert_eq!(paths.home(), Path::new("relative-home"));
        assert_eq!(paths.state_dir(), Path::new("relative-home/.dof"));
        assert_eq!(paths.workspace(), Path::new("relative-home/.dof/workspace"));
        assert_eq!(paths.config(), Path::new("relative-home/.dof/config.yaml"));
        assert_eq!(paths.bin(), Path::new("relative-home/.dof/bin"));
        assert_eq!(paths.backups(), Path::new("relative-home/.dof/backups"));
    }

    #[test]
    fn state_checks_never_follow_a_state_symlink() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let external = root.path().join("external");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&external).unwrap();
        symlink(&external, home.join(".dof")).unwrap();
        let paths = DofPaths::from_home(home);

        let error = paths.require_state_dir().unwrap_err();
        assert!(format!("{error:#}").contains("not a real directory"));
        assert!(paths.ensure_state_dir().is_err());
    }
}
