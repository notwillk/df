use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args as ClapArgs;
use gix::bstr::ByteSlice;

use crate::config::{Config, ConfigStore, RepoConfig};
use crate::state::DofPaths;

struct ExistingRepository {
    url: String,
    endpoint_fingerprint: String,
    remote_branch: String,
}

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Repository to install or update
    #[arg(value_name = "REPOSITORY")]
    repository: String,

    /// Branch to clone or update
    #[arg(short = 'b', long, value_name = "BRANCH", default_value = "main")]
    branch: String,

    /// Replace an existing dof workspace and config
    #[arg(long)]
    force: bool,
}

pub(crate) fn run(args: Args) -> Result<()> {
    let paths = DofPaths::from_env()?;
    paths.ensure_state_dir()?;
    let _state_lock = lock_state_dir(&paths)?;

    let workspace_exists = path_exists(paths.workspace())?;
    let config_exists = path_exists(paths.config())?;

    if args.force {
        remove_path(paths.workspace())
            .with_context(|| format!("failed to remove {}", paths.workspace().display()))?;
        remove_path(paths.config())
            .with_context(|| format!("failed to remove {}", paths.config().display()))?;
        install_fresh(&paths, &args)?;
    } else {
        match (workspace_exists, config_exists) {
            (false, false) => install_fresh(&paths, &args)?,
            (true, true) => update_existing(&paths, &args)?,
            _ => bail!(
                "dof state is incomplete: workspace and config must either both exist or both be absent; use --force to replace it"
            ),
        }
    }

    println!("{}", paths.workspace().display());
    Ok(())
}

fn lock_state_dir(paths: &DofPaths) -> Result<File> {
    let directory = File::open(paths.state_dir()).with_context(|| {
        format!(
            "failed to open dof state directory {} for locking",
            paths.state_dir().display()
        )
    })?;
    directory.lock().with_context(|| {
        format!(
            "failed to lock dof state directory {}",
            paths.state_dir().display()
        )
    })?;
    Ok(directory)
}

fn install_fresh(paths: &DofPaths, args: &Args) -> Result<()> {
    let workspace = claim_workspace(paths.workspace())?;

    let endpoint_fingerprint =
        match clone_repository(&args.repository, &args.branch, workspace.path()) {
            Ok(fingerprint) => fingerprint,
            Err(error) => return Err(cleanup_workspace_error(error, &workspace)),
        };

    let config = Config::new(RepoConfig::new(
        args.repository.to_owned(),
        args.branch.to_owned(),
        endpoint_fingerprint,
    ));
    if let Err(error) = ConfigStore::new(paths).create(&config) {
        return Err(cleanup_workspace_error(error, &workspace));
    }

    Ok(())
}

fn update_existing(paths: &DofPaths, args: &Args) -> Result<()> {
    let config = ConfigStore::new(paths).read()?;
    if config.repo.url != args.repository
        || branch_ref(&config.repo.branch) != branch_ref(&args.branch)
    {
        bail!(
            "dof is already configured for a different repository or branch; use --force to replace it"
        );
    }

    let existing = validate_existing_workspace(
        paths.workspace(),
        &args.repository,
        &args.branch,
        config.repo.endpoint_fingerprint.as_deref(),
    )?;
    fast_forward_workspace(paths.workspace(), &existing)
}

fn validate_existing_workspace(
    workspace: &Path,
    repository: &str,
    branch: &str,
    endpoint_fingerprint: Option<&str>,
) -> Result<ExistingRepository> {
    require_real_directory(workspace, "dof workspace")?;
    require_real_directory(&workspace.join(".git"), "dof workspace Git directory")?;

    let repo = gix::open_opts(workspace, gix::open::Options::isolated())
        .with_context(|| format!("failed to open dof workspace {}", workspace.display()))?;
    let expected_workdir = fs::canonicalize(workspace)
        .with_context(|| format!("failed to resolve dof workspace {}", workspace.display()))?;
    let actual_workdir = repo
        .workdir()
        .context("dof workspace Git metadata does not define a worktree")?
        .canonicalize()
        .context("failed to resolve the worktree configured by dof workspace Git metadata")?;
    if actual_workdir != expected_workdir {
        bail!(
            "dof workspace Git metadata points to a different worktree; use --force to replace it"
        );
    }
    let expected_branch = branch_ref(branch);
    let head = repo
        .head()
        .context("failed to inspect dof workspace HEAD")?;
    if head
        .referent_name()
        .is_none_or(|name| name.as_bstr() != expected_branch.as_bytes())
    {
        bail!(
            "dof workspace is not checked out on configured branch '{branch}'; use --force to replace it"
        );
    }
    if head.id().is_none() {
        bail!("dof workspace branch '{branch}' does not contain a commit");
    }

    let expected_url = canonical_repository_url(repository)?;
    let expected_remote = repo
        .remote_at(expected_url.clone())
        .context("failed to resolve configured repository remote")?;
    let local_branch = repo
        .find_reference(expected_branch.as_str())
        .context("failed to find the configured local branch")?;
    let remote = local_branch
        .remote(gix::remote::Direction::Fetch)
        .transpose()
        .context("failed to find the configured fetch remote")?
        .context("dof workspace branch does not have a configured fetch remote")?;
    let remote_branch = local_branch
        .remote_ref_name(gix::remote::Direction::Fetch)
        .transpose()
        .context("failed to inspect the configured remote branch")?
        .context("dof workspace branch does not have a configured remote branch")?;
    let tracking_branch = local_branch
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)
        .transpose()
        .context("failed to inspect the configured remote-tracking branch")?
        .context("dof workspace branch does not have a configured remote-tracking branch")?;
    let remote_name = remote
        .name()
        .and_then(gix::remote::Name::as_symbol)
        .context("dof workspace branch does not use a named fetch remote")?;
    if remote_branch.as_bstr() != expected_branch.as_bytes() {
        bail!("dof workspace branch tracks a different remote branch; use --force to replace it");
    }
    if remote.url(gix::remote::Direction::Fetch)
        != expected_remote.url(gix::remote::Direction::Fetch)
    {
        bail!(
            "dof workspace remote does not match the configured repository; use --force to replace it"
        );
    }

    let remote_branch = remote_branch
        .as_bstr()
        .to_str()
        .context("configured remote branch name is not valid UTF-8")?;
    let tracking_branch = tracking_branch
        .as_bstr()
        .to_str()
        .context("configured remote-tracking branch name is not valid UTF-8")?;
    let branch_name = expected_branch
        .strip_prefix("refs/heads/")
        .expect("branch_ref always produces a local branch reference");
    let expected_tracking_branch = format!("refs/remotes/{remote_name}/{branch_name}");
    if tracking_branch != expected_tracking_branch {
        bail!(
            "dof workspace branch uses an unsafe remote-tracking reference; use --force to replace it"
        );
    }
    Ok(ExistingRepository {
        url: repository.to_owned(),
        endpoint_fingerprint: endpoint_fingerprint
            .map(str::to_owned)
            .unwrap_or_else(|| repository_endpoint_fingerprint(&expected_url)),
        remote_branch: remote_branch.to_owned(),
    })
}

fn canonical_repository_url(repository: &str) -> Result<gix::Url> {
    let url = gix::url::parse(repository.as_bytes())
        .context("failed to parse configured repository URL")?;
    canonicalize_repository_url(url)
}

fn canonicalize_repository_url(mut url: gix::Url) -> Result<gix::Url> {
    let current_dir = std::env::current_dir().context("failed to resolve current directory")?;
    url.canonicalize(&current_dir)
        .context("failed to resolve configured repository URL")?;
    Ok(url)
}

fn repository_endpoint_fingerprint(url: &gix::Url) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut endpoint = url.clone();
    if matches!(
        endpoint.scheme,
        gix::url::Scheme::Http | gix::url::Scheme::Https
    ) {
        endpoint.set_user(None);
    }
    endpoint.set_password(None);
    let serialized = endpoint.to_bstring();
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, serialized.as_ref());
    let mut fingerprint = String::with_capacity("sha256:".len() + digest.as_ref().len() * 2);
    fingerprint.push_str("sha256:");
    for byte in digest.as_ref() {
        fingerprint.push(char::from(HEX[usize::from(byte >> 4)]));
        fingerprint.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    fingerprint
}

fn require_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        bail!("{label} {} is not a real directory", path.display());
    }
    Ok(())
}

fn fast_forward_workspace(workspace: &Path, repository: &ExistingRepository) -> Result<()> {
    require_expected_repository_endpoint(workspace, repository)?;
    let local = rev_parse(workspace, "HEAD^{commit}")?;
    let advertised = remote_tip(workspace, repository)?;

    let mut fetch = git_command(workspace);
    fetch.args([
        "-c",
        "core.hooksPath=/dev/null",
        "fetch",
        "--no-write-fetch-head",
        "--no-tags",
        "--no-recurse-submodules",
        "--",
        &repository.url,
        &repository.remote_branch,
    ]);
    require_git_status(
        fetch.status().context("failed to execute Git fetch")?,
        "fetch repository updates",
    )?;

    ensure_head_unchanged(workspace, &local)?;
    if remote_tip(workspace, repository)? != advertised {
        bail!("configured remote branch changed while it was being fetched; retry the command");
    }
    let remote = rev_parse(workspace, &format!("{advertised}^{{commit}}"))?;
    if remote != advertised {
        bail!("configured remote branch did not resolve to the advertised commit");
    }
    if local == remote || is_ancestor(workspace, &remote, &local)? {
        return Ok(());
    }
    if !is_ancestor(workspace, &local, &remote)? {
        bail!("configured branch has diverged and cannot be fast-forwarded");
    }

    ensure_head_unchanged(workspace, &local)?;
    let mut merge = git_command(workspace);
    merge.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "submodule.recurse=false",
        "merge",
        "--quiet",
        "--ff-only",
        "--no-edit",
        "--no-autostash",
        "--no-overwrite-ignore",
        &remote,
    ]);
    require_git_status(
        merge
            .status()
            .context("failed to execute Git fast-forward")?,
        "fast-forward the configured branch",
    )?;

    let updated = rev_parse(workspace, "HEAD^{commit}")?;
    if updated != remote {
        bail!(
            "Git reported success but did not update the configured branch to the fetched commit"
        );
    }
    Ok(())
}

fn require_expected_repository_endpoint(
    workspace: &Path,
    repository: &ExistingRepository,
) -> Result<()> {
    let mut command = git_command(workspace);
    command.args(["ls-remote", "--get-url", "--", &repository.url]);
    let output = command
        .output()
        .context("failed to execute Git repository URL lookup")?;
    let stdout = require_git_success(output, "resolve the configured repository URL")?;
    let resolved = stdout.strip_suffix('\n').unwrap_or(&stdout);
    let resolved = resolved.strip_suffix('\r').unwrap_or(resolved);
    if resolved.contains(['\n', '\r']) {
        bail!("Git reported more than one resolved repository URL");
    }
    let resolved = canonical_repository_url(resolved)
        .context("failed to parse the repository URL resolved by Git")?;
    let resolved_fingerprint = repository_endpoint_fingerprint(&resolved);
    if resolved_fingerprint != repository.endpoint_fingerprint {
        bail!(
            "the configured repository URL resolves to a different endpoint than this dof installation; refusing to update"
        );
    }
    Ok(())
}

fn remote_tip(workspace: &Path, repository: &ExistingRepository) -> Result<String> {
    let mut command = git_command(workspace);
    command
        .args([
            "ls-remote",
            "--exit-code",
            "--refs",
            "--",
            &repository.url,
            &repository.remote_branch,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let output = command
        .output()
        .context("failed to execute Git remote branch lookup")?;
    require_git_status(output.status, "resolve the configured remote branch")?;
    let stdout = String::from_utf8(output.stdout)
        .context("Git remote branch lookup returned output that is not valid UTF-8")?;
    let mut matches = stdout.lines().filter_map(|line| {
        let (oid, name) = line.split_once('\t')?;
        (name == repository.remote_branch).then_some(oid)
    });
    let oid = matches
        .next()
        .context("Git did not report the configured remote branch")?;
    if matches.next().is_some() {
        bail!("Git reported the configured remote branch more than once");
    }
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git reported an invalid object ID for the configured remote branch");
    }
    Ok(oid.to_ascii_lowercase())
}

fn is_ancestor(workspace: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let mut command = git_command(workspace);
    command.args(["merge-base", "--is-ancestor", ancestor, descendant]);
    let status = command
        .status()
        .context("failed to execute Git ancestry check")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("Git failed while checking repository ancestry"),
    }
}

fn ensure_head_unchanged(workspace: &Path, expected: &str) -> Result<()> {
    if rev_parse(workspace, "HEAD^{commit}")? != expected {
        bail!("dof workspace HEAD changed while it was being updated; retry the command");
    }
    Ok(())
}

fn rev_parse(workspace: &Path, revision: &str) -> Result<String> {
    let mut command = git_command(workspace);
    command.args(["rev-parse", "--verify", revision]);
    let output = command
        .output()
        .context("failed to execute Git revision lookup")?;
    let stdout = require_git_success(output, "resolve a Git revision")?;
    Ok(stdout.trim().to_owned())
}

fn require_git_success(output: Output, operation: &str) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        bail!("Git failed to {operation}");
    }
    bail!("Git failed to {operation}: {detail}")
}

fn require_git_status(status: ExitStatus, operation: &str) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    if let Some(code) = status.code() {
        bail!("Git failed to {operation} with exit code {code}");
    }
    bail!("Git failed to {operation} after being terminated by a signal")
}

fn git_command(workspace: &Path) -> Command {
    let mut command = Command::new("git");
    for variable in GIT_LOCAL_ENVIRONMENT {
        command.env_remove(variable);
    }
    command
        .arg("--git-dir")
        .arg(workspace.join(".git"))
        .arg("--work-tree")
        .arg(workspace);
    command
}

const GIT_LOCAL_ENVIRONMENT: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

fn branch_ref(branch: &str) -> String {
    if branch.starts_with("refs/heads/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    }
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

fn clone_repository(repository: &str, branch: &str, workspace: &Path) -> Result<String> {
    let branch_ref = branch_ref(branch);
    let mut permissions = gix::open::Permissions::all();
    permissions.config.git_binary = true;
    permissions.config.env = false;
    let mut fetch = gix::clone::PrepareFetch::new(
        repository,
        workspace,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        gix::open::Options::default().permissions(permissions),
    )
    .context("failed to prepare repository clone")?
    .with_ref_name(Some(branch_ref.as_str()))
    .context("invalid branch name")?;
    let should_interrupt = AtomicBool::new(false);
    let (mut checkout, _) = fetch
        .fetch_then_checkout(gix::progress::Discard, &should_interrupt)
        .context("failed to fetch repository")?;
    let (repo, _) = checkout
        .main_worktree(gix::progress::Discard, &should_interrupt)
        .context("failed to check out repository")?;

    let local_branch = repo
        .find_reference(branch_ref.as_str())
        .context("failed to find the cloned local branch")?;
    let remote = local_branch
        .remote(gix::remote::Direction::Fetch)
        .transpose()
        .context("failed to find the cloned fetch remote")?
        .context("cloned branch does not have a configured fetch remote")?;
    let effective_url = remote
        .url(gix::remote::Direction::Fetch)
        .context("cloned fetch remote does not have a URL")?
        .clone();
    let effective_url = canonicalize_repository_url(effective_url)
        .context("failed to resolve effective repository URL")?;
    Ok(repository_endpoint_fingerprint(&effective_url))
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
    fn endpoint_fingerprints_ignore_http_credentials_but_preserve_ssh_users() {
        let first_https = gix::url::parse(b"https://first:first-secret@example.com/repo").unwrap();
        let second_https =
            gix::url::parse(b"https://second:second-secret@example.com/repo").unwrap();
        assert_eq!(
            repository_endpoint_fingerprint(&first_https),
            repository_endpoint_fingerprint(&second_https)
        );

        let first_ssh = gix::url::parse(b"first@example.com:repo").unwrap();
        let second_ssh = gix::url::parse(b"second@example.com:repo").unwrap();
        assert_ne!(
            repository_endpoint_fingerprint(&first_ssh),
            repository_endpoint_fingerprint(&second_ssh)
        );
    }

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
