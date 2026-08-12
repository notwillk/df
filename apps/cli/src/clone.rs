use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args as ClapArgs;

use crate::config::{Config, ConfigStore, RepoConfig};
use crate::state::DofPaths;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Repository to clone
    #[arg(value_name = "REPOSITORY")]
    repository: String,

    /// Branch to clone
    #[arg(short = 'b', long, value_name = "BRANCH", default_value = "main")]
    branch: String,

    /// Replace an existing dof workspace and config
    #[arg(long)]
    force: bool,
}

pub(crate) fn run(args: Args) -> Result<()> {
    let paths = DofPaths::from_env()?;
    paths.ensure_state_dir()?;

    let workspace_exists = path_exists(paths.workspace())?;
    let config_exists = path_exists(paths.config())?;

    if !args.force && (workspace_exists || config_exists) {
        let conflicts = [
            workspace_exists.then(|| paths.workspace().display().to_string()),
            config_exists.then(|| paths.config().display().to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");

        bail!("dof state already exists at {conflicts}; use --force to replace it");
    }

    if args.force {
        remove_path(paths.workspace())
            .with_context(|| format!("failed to remove {}", paths.workspace().display()))?;
        remove_path(paths.config())
            .with_context(|| format!("failed to remove {}", paths.config().display()))?;
    }

    let workspace = claim_workspace(paths.workspace())?;

    if let Err(error) = clone_repository(&args.repository, &args.branch, workspace.path()) {
        return Err(cleanup_workspace_error(error, &workspace));
    }

    let config = Config::new(RepoConfig::new(args.repository, args.branch));
    if let Err(error) = ConfigStore::new(&paths).create(&config) {
        return Err(cleanup_workspace_error(error, &workspace));
    }

    println!("{}", paths.workspace().display());
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

struct WorkspaceClaim {
    path: PathBuf,
    directory: File,
}

impl WorkspaceClaim {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn claim_workspace(path: &Path) -> Result<WorkspaceClaim> {
    fs::create_dir(path).with_context(|| {
        format!(
            "failed to create workspace {}; another dof clone may be running",
            path.display()
        )
    })?;

    // Keep the claimed directory open for the lifetime of the clone. Besides
    // identifying the directory, the open handle prevents its inode from being
    // reused if another process replaces the workspace path before cleanup.
    let directory = File::open(path)
        .with_context(|| format!("failed to open claimed workspace {}", path.display()))?;
    let opened = directory
        .metadata()
        .with_context(|| format!("failed to inspect claimed workspace {}", path.display()))?;
    let current = fs::symlink_metadata(path)
        .with_context(|| format!("failed to re-inspect claimed workspace {}", path.display()))?;
    if !same_directory(&opened, &current) {
        bail!(
            "workspace {} changed while it was being claimed; refusing to use it",
            path.display()
        );
    }

    Ok(WorkspaceClaim {
        path: path.to_owned(),
        directory,
    })
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove file {}", path.display()))
    }
}

fn clone_repository(repository: &str, branch: &str, workspace: &Path) -> Result<()> {
    let branch_ref = if branch.starts_with("refs/heads/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    };

    let mut fetch = gix::prepare_clone(repository, workspace)
        .context("failed to prepare repository clone")?
        .with_ref_name(Some(branch_ref.as_str()))
        .context("invalid branch name")?;
    let should_interrupt = AtomicBool::new(false);
    let (mut checkout, _) = fetch
        .fetch_then_checkout(gix::progress::Discard, &should_interrupt)
        .context("failed to fetch repository")?;
    checkout
        .main_worktree(gix::progress::Discard, &should_interrupt)
        .context("failed to check out repository")?;

    Ok(())
}

fn cleanup_workspace_error(error: anyhow::Error, workspace: &WorkspaceClaim) -> anyhow::Error {
    match remove_claimed_workspace(workspace) {
        Ok(()) => error,
        Err(cleanup_error) => anyhow!("{error:#}; failed to clean up workspace: {cleanup_error:#}"),
    }
}

fn remove_claimed_workspace(workspace: &WorkspaceClaim) -> Result<()> {
    let current = match fs::symlink_metadata(workspace.path()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect claimed workspace {}",
                    workspace.path().display()
                )
            });
        }
    };
    let claimed = workspace.directory.metadata().with_context(|| {
        format!(
            "failed to inspect claimed workspace {}",
            workspace.path().display()
        )
    })?;
    if !same_directory(&claimed, &current) {
        bail!(
            "workspace {} was replaced after this clone claimed it; refusing to remove the replacement",
            workspace.path().display()
        );
    }

    fs::remove_dir_all(workspace.path())
        .with_context(|| format!("failed to remove directory {}", workspace.path().display()))
}

fn same_directory(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type().is_dir()
        && right.file_type().is_dir()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_preserves_a_workspace_that_replaced_the_claim() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        let claim = claim_workspace(&path).unwrap();

        fs::remove_dir(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep.txt"), "replacement\n").unwrap();

        let error = cleanup_workspace_error(anyhow!("clone failed"), &claim);

        let message = format!("{error:#}");
        assert!(message.contains("failed to clean up workspace"));
        assert!(message.contains("refusing to remove the replacement"));
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "replacement\n"
        );
    }
}
