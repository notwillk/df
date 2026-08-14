use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::backup::BackupStore;
use crate::config::{Config, ConfigStore};
use crate::home_fs::{CopySource, ExpectedState, HomeFs, ObservedTarget, StageSource};
use crate::state::DofPaths;
use crate::workspace::{self, ApplyScript, HomePath, Target, ValidatedWorkspace};

pub(crate) fn apply() -> Result<()> {
    let paths = DofPaths::from_env()?;
    let home = HomeFs::new(paths.home())?;
    paths.require_state_dir()?;

    let config = ConfigStore::new(&paths).read()?;
    let workspace = workspace::build_manifest(paths.workspace())?;
    let plan = ApplyPlan::build(&home, paths.backups(), &config, workspace)?;
    let result = plan.execute(&home, paths.backups())?;

    println!("applied: {}", result.applied);
    println!("unchanged: {}", result.unchanged);
    if let Some(snapshot) = result.backup_snapshot {
        println!("backup: {}", snapshot.display());
    }

    Ok(())
}

/// The enabled projection of a fully validated workspace.
///
/// Ownership is compiled before this projection, so disabled features still
/// participate in linting while only enabled declarations reach reconciliation.
struct ActiveWorkspace {
    directories: BTreeSet<HomePath>,
    copies: Vec<(HomePath, PathBuf)>,
    drop_ins: Vec<(HomePath, Vec<u8>)>,
    snippets: Vec<(HomePath, Vec<String>)>,
    scripts: Vec<PlannedScript>,
}

impl ActiveWorkspace {
    fn project(workspace: ValidatedWorkspace, config: &Config) -> Self {
        let (targets, scripts) = workspace.into_parts();
        let mut directories = BTreeSet::new();
        let mut copies = Vec::new();
        let mut drop_ins = Vec::new();
        let mut snippets = Vec::new();

        for (path, target) in targets {
            match target {
                Target::Directory { features } => {
                    if features
                        .iter()
                        .any(|feature| config.feature_enabled(feature))
                    {
                        directories.insert(path);
                    }
                }
                Target::CopyFile { feature, source } => {
                    if config.feature_enabled(&feature) {
                        copies.push((path, source));
                    }
                }
                Target::DropIns { fragments } => {
                    let mut contents = Vec::new();
                    for fragment in fragments {
                        if config.feature_enabled(&fragment.feature) {
                            contents.extend_from_slice(&fragment.contents);
                        }
                    }
                    if !contents.is_empty() {
                        drop_ins.push((path, contents));
                    }
                }
                Target::Snippets { contributions } => {
                    let mut enabled = false;
                    let mut strings = Vec::new();
                    for (feature, contribution) in contributions {
                        if config.feature_enabled(&feature) {
                            enabled = true;
                            strings.extend(contribution);
                        }
                    }
                    if enabled {
                        snippets.push((path, strings));
                    }
                }
            }
        }

        let mut scripts = scripts
            .into_iter()
            .filter(|script| config.feature_enabled(&script.feature))
            .map(PlannedScript::from)
            .collect::<Vec<_>>();
        scripts.sort_by(|left, right| left.feature_name.cmp(&right.feature_name));

        Self {
            directories,
            copies,
            drop_ins,
            snippets,
            scripts,
        }
    }
}

struct ApplyPlan {
    missing_directories: Vec<HomePath>,
    changes: Vec<FileChange>,
    scripts: Vec<PlannedScript>,
    unchanged: usize,
}

struct FileChange {
    target: HomePath,
    contents: DesiredContents,
    mode: u32,
    expected: ExpectedState,
}

enum DesiredContents {
    Copy(CopySource),
    Generated(Vec<u8>),
}

struct PlannedScript {
    feature_name: String,
    path: PathBuf,
}

impl From<ApplyScript> for PlannedScript {
    fn from(script: ApplyScript) -> Self {
        Self {
            feature_name: script.feature,
            path: script.path,
        }
    }
}

struct ApplyResult {
    applied: usize,
    unchanged: usize,
    backup_snapshot: Option<PathBuf>,
}

impl ApplyPlan {
    fn build(
        home: &HomeFs,
        backup_root: &Path,
        config: &Config,
        workspace: ValidatedWorkspace,
    ) -> Result<Self> {
        let mut active = ActiveWorkspace::project(workspace, config);
        let mut changes = Vec::new();
        let mut unchanged = 0;

        // Copy declarations reconcile first and retain their source identity so
        // staging can prove the source did not change after workspace linting.
        for (target, source_path) in active.copies {
            home.preflight_file_parent(&target)?;
            let source = home.inspect_source(source_path)?;
            let expected = match home.observe(&target)? {
                ObservedTarget::Regular {
                    expected,
                    mode,
                    length,
                } => {
                    if source.mode() == mode
                        && source.length() == length
                        && home.contents_equal(&source, &target, &expected)?
                    {
                        unchanged += 1;
                        continue;
                    }
                    expected
                }
                ObservedTarget::Symlink { expected } => expected,
                ObservedTarget::Directory => bail!(
                    "file destination {} is an existing directory",
                    home.path(&target).display()
                ),
                ObservedTarget::Unsupported => bail!(
                    "file destination {} has an unsupported file type",
                    home.path(&target).display()
                ),
                ObservedTarget::Missing => ExpectedState::Missing,
            };
            let mode = source.mode();
            changes.push(FileChange {
                target,
                contents: DesiredContents::Copy(source),
                mode,
                expected,
            });
        }

        // Drop-ins own the complete generated file. Their fragments were
        // globally validated and sorted before enabled-feature projection.
        for (target, contents) in active.drop_ins {
            home.preflight_file_parent(&target)?;
            let (mode, expected) = match home.observe(&target)? {
                ObservedTarget::Regular {
                    expected,
                    mode,
                    length,
                } => {
                    if length == contents.len() as u64
                        && home.contents_equal_bytes(&contents, &target, &expected)?
                    {
                        unchanged += 1;
                        continue;
                    }
                    (mode, expected)
                }
                ObservedTarget::Symlink { expected } => (0o600, expected),
                ObservedTarget::Directory => bail!(
                    "drop-in destination {} is an existing directory",
                    home.path(&target).display()
                ),
                ObservedTarget::Unsupported => bail!(
                    "drop-in destination {} has an unsupported file type",
                    home.path(&target).display()
                ),
                ObservedTarget::Missing => (0o600, ExpectedState::Missing),
            };

            active.directories.extend(target.parents());
            changes.push(FileChange {
                target,
                contents: DesiredContents::Generated(contents),
                mode,
                expected,
            });
        }

        // Snippet rendering is pure. Every enabled feature contributes in
        // lexical feature order, which was preserved by the compiler's map.
        for (target, required) in active.snippets {
            home.preflight_file_parent(&target)?;
            let (original, mode, expected, existed) = match home.observe(&target)? {
                ObservedTarget::Regular { expected, mode, .. } => {
                    (home.read_text(&target, &expected)?, mode, expected, true)
                }
                ObservedTarget::Symlink { .. } => bail!(
                    "snippet target {} is a symlink; snippet targets must be regular files",
                    home.path(&target).display()
                ),
                ObservedTarget::Directory => bail!(
                    "snippet target {} is an existing directory",
                    home.path(&target).display()
                ),
                ObservedTarget::Unsupported => bail!(
                    "snippet target {} has an unsupported file type",
                    home.path(&target).display()
                ),
                ObservedTarget::Missing => (String::new(), 0o600, ExpectedState::Missing, false),
            };
            let rendered = render_snippets(&original, &required);
            if rendered == original {
                if existed {
                    unchanged += 1;
                }
                continue;
            }

            active.directories.extend(target.parents());
            changes.push(FileChange {
                target,
                contents: DesiredContents::Generated(rendered.into_bytes()),
                mode,
                expected,
            });
        }

        let mut directories = active.directories.into_iter().collect::<Vec<_>>();
        directories.sort_by(|left, right| {
            left.as_path()
                .components()
                .count()
                .cmp(&right.as_path().components().count())
                .then_with(|| left.cmp(right))
        });
        let mut missing_directories = Vec::new();
        for target in directories {
            if home.preflight_directory(&target)? {
                missing_directories.push(target);
            }
        }

        if changes
            .iter()
            .any(|change| !matches!(change.expected, ExpectedState::Missing))
        {
            BackupStore::preflight(backup_root)?;
        }

        Ok(Self {
            missing_directories,
            changes,
            scripts: active.scripts,
            unchanged,
        })
    }

    fn execute(self, home: &HomeFs, backup_root: &Path) -> Result<ApplyResult> {
        for target in &self.missing_directories {
            home.create_directory(target)?;
        }

        let mut backups = BackupStore::new(backup_root);
        let mut applied = 0;
        for change in self.changes {
            let source = match &change.contents {
                DesiredContents::Copy(source) => StageSource::Copy(source),
                DesiredContents::Generated(contents) => StageSource::Bytes(contents),
            };
            let staged = home.stage(&change.target, source, change.mode)?;

            if !matches!(change.expected, ExpectedState::Missing) {
                home.revalidate(&change.target, &change.expected)?;
                backups.back_up(home, &change.target, &change.expected)?;
            }
            // install performs a final identity check after backup, closing
            // the most consequential plan/execution race without an apply lock.
            home.install(&change.target, staged, &change.expected)?;
            applied += 1;
        }

        for script in self.scripts {
            run_apply_script(&script)?;
        }

        Ok(ApplyResult {
            applied,
            unchanged: self.unchanged,
            backup_snapshot: backups.snapshot().map(Path::to_owned),
        })
    }
}

fn render_snippets(existing: &str, snippets: &[String]) -> String {
    let mut rendered = existing.to_owned();
    for snippet in snippets {
        if !rendered.contains(snippet) {
            append_snippet(&mut rendered, snippet);
        }
    }
    rendered
}

fn append_snippet(contents: &mut String, snippet: &str) {
    if !contents.is_empty() && !contents.ends_with('\n') && !snippet.starts_with('\n') {
        contents.push('\n');
    }
    contents.push_str(snippet);
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
}

fn run_apply_script(script: &PlannedScript) -> Result<()> {
    workspace::validate_apply_script_path(&script.feature_name, &script.path)?;
    let working_directory = script.path.parent().with_context(|| {
        format!(
            "apply script for feature '{}' has no parent directory",
            script.feature_name
        )
    })?;
    let status = Command::new("./apply")
        .current_dir(working_directory)
        .status()
        .with_context(|| {
            format!(
                "failed to run apply script for feature '{}' at {}",
                script.feature_name,
                script.path.display()
            )
        })?;
    if !status.success() {
        bail!(
            "apply script for feature '{}' failed with {status}",
            script.feature_name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render_snippets;

    #[test]
    fn snippet_rendering_is_idempotent_for_multiline_blocks() {
        let block = concat!(
            "if command -v mise >/dev/null 2>&1; then\n",
            "  eval \"$(mise activate bash)\"\n",
            "fi"
        )
        .to_owned();
        let snippets = vec!["export EDITOR=vim".to_owned(), block.clone()];

        let rendered = render_snippets("# existing", &snippets);
        assert_eq!(
            rendered,
            concat!(
                "# existing\n",
                "export EDITOR=vim\n",
                "if command -v mise >/dev/null 2>&1; then\n",
                "  eval \"$(mise activate bash)\"\n",
                "fi\n"
            )
        );
        assert_eq!(render_snippets(&rendered, &snippets), rendered);
        assert_eq!(rendered.matches(&block).count(), 1);
    }

    #[test]
    fn snippet_rendering_respects_existing_newline_boundaries() {
        let rendered = render_snippets(
            "existing-without-newline",
            &["\nalready-separated\n".to_owned()],
        );
        assert_eq!(rendered, "existing-without-newline\nalready-separated\n");
    }
}
