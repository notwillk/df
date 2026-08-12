use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::state::DofPaths;

pub(crate) fn run(script: &OsStr, arguments: &[OsString]) -> Result<()> {
    validate_script_name(script)?;

    let paths = DofPaths::from_env()?;
    paths.require_bin_dir()?;

    let script_path = paths.bin().join(script);
    let metadata = fs::symlink_metadata(&script_path)
        .with_context(|| format!("failed to inspect dof script {}", script_path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("dof script {} is not a regular file", script_path.display());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("dof script {} is not executable", script_path.display());
    }

    let error = Command::new(&script_path).args(arguments).exec();
    Err(error).with_context(|| format!("failed to execute dof script {}", script_path.display()))
}

fn validate_script_name(script: &OsStr) -> Result<()> {
    let bytes = script.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        bail!(
            "dof script name {:?} must be a single file name without path separators",
            script
        );
    }
    Ok(())
}
