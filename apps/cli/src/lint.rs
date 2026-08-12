use std::path::Path;

use anyhow::Result;

use crate::workspace;

pub(crate) fn lint_directory(root: &Path) -> Result<()> {
    workspace::build_manifest_following_root(root).map(|_| ())
}
