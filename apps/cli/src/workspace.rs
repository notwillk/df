use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_yaml_ng::Value;

use crate::drop_ins::{self, DropInFragment};

#[derive(Debug)]
pub(crate) struct FeatureDirectory {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

/// A non-empty path beneath HOME that cannot address dof's own state.
///
/// Construction accepts arbitrary Unix path bytes so payloads discovered in a
/// feature's `home/` tree do not need to be valid UTF-8.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct HomePath(PathBuf);

impl HomePath {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            bail!(
                "HOME path '{}' must be a non-empty relative path containing only normal components",
                path.display()
            );
        }

        // `Path::components` safely removes redundant separators and interior
        // `.` components. Rebuild the stored value from those components so a
        // spelling such as `foo/.` cannot reach filesystem operations as a
        // directory-shaped file target. This also preserves arbitrary Unix
        // bytes in each normal component.
        let mut normalized = PathBuf::new();
        for component in path.components() {
            let Component::Normal(component) = component else {
                bail!(
                    "HOME path '{}' must be a non-empty relative path containing only normal components",
                    path.display()
                );
            };
            normalized.push(component);
        }
        if normalized.as_os_str().is_empty() {
            bail!(
                "HOME path '{}' must be a non-empty relative path containing only normal components",
                path.display()
            );
        }
        if normalized
            .components()
            .next()
            .is_some_and(|component| component.as_os_str() == OsStr::new(".dof"))
        {
            bail!("HOME path '{}' may not manage dof state", path.display());
        }
        Ok(Self(normalized))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    /// Returns non-empty ancestors in shallow-to-deep order.
    pub(crate) fn parents(&self) -> impl Iterator<Item = Self> {
        let component_count = self.0.components().count();
        let mut path = PathBuf::new();
        let mut parents = Vec::with_capacity(component_count.saturating_sub(1));
        for (index, component) in self.0.components().enumerate() {
            if index + 1 == component_count {
                break;
            }
            path.push(component.as_os_str());
            parents.push(Self(path.clone()));
        }
        parents.into_iter()
    }
}

impl AsRef<Path> for HomePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for HomePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// A completely parsed and ownership-checked workspace.
///
/// Its fields stay private so callers cannot accidentally bypass compilation.
#[derive(Debug)]
pub(crate) struct ValidatedWorkspace {
    targets: TargetIndex,
    apply_scripts: Vec<ApplyScript>,
}

impl ValidatedWorkspace {
    pub(crate) fn into_parts(self) -> (TargetIndex, Vec<ApplyScript>) {
        (self.targets, self.apply_scripts)
    }
}

#[derive(Debug)]
pub(crate) struct TargetIndex(BTreeMap<HomePath, Target>);

impl IntoIterator for TargetIndex {
    type Item = (HomePath, Target);
    type IntoIter = std::collections::btree_map::IntoIter<HomePath, Target>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[derive(Debug)]
pub(crate) enum Target {
    Directory {
        features: BTreeSet<String>,
    },
    CopyFile {
        feature: String,
        source: PathBuf,
    },
    Snippets {
        contributions: BTreeMap<String, Vec<String>>,
    },
    DropIns {
        fragments: Vec<DropInFragment>,
    },
}

#[derive(Debug)]
pub(crate) struct ApplyScript {
    pub(crate) feature: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug)]
struct Claim {
    path: HomePath,
    feature: String,
    kind: ClaimKind,
}

#[derive(Clone, Debug)]
enum ClaimKind {
    Directory,
    CopyFile { source: PathBuf },
    Snippet { strings: Vec<String> },
    DropIn { fragment: DropInFragment },
}

/// Discovers real directories beneath `<workspace>/features` in name order.
///
/// A missing features directory means the workspace has no features. A
/// present features path must be a real directory and is never followed.
pub(crate) fn discover_features(workspace: &Path) -> Result<Vec<FeatureDirectory>> {
    discover_features_with_root_policy(workspace, false)
}

fn discover_features_following_root(workspace: &Path) -> Result<Vec<FeatureDirectory>> {
    discover_features_with_root_policy(workspace, true)
}

fn discover_features_with_root_policy(
    workspace: &Path,
    follow_root_symlink: bool,
) -> Result<Vec<FeatureDirectory>> {
    let metadata = if follow_root_symlink {
        fs::metadata(workspace)
    } else {
        fs::symlink_metadata(workspace)
    }
    .with_context(|| format!("failed to inspect workspace {}", workspace.display()))?;
    if !metadata.is_dir() {
        bail!("workspace {} is not a real directory", workspace.display());
    }

    let features_root = workspace.join("features");
    let metadata = match fs::symlink_metadata(&features_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect features directory {}",
                    features_root.display()
                )
            });
        }
    };
    if !metadata.file_type().is_dir() {
        bail!(
            "features directory {} is not a real directory",
            features_root.display()
        );
    }

    let mut entries = fs::read_dir(&features_root)
        .with_context(|| {
            format!(
                "failed to read features directory {}",
                features_root.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read entries in {}", features_root.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut features = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect feature path {}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let name = name.into_string().map_err(|name| {
            anyhow!(
                "feature directory name is not valid UTF-8: {}",
                Path::new(&name).display()
            )
        })?;
        if name.chars().any(char::is_control) {
            bail!("feature directory name contains control characters");
        }

        features.push(FeatureDirectory { name, path });
    }

    features.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(features)
}

/// Parses and validates every feature without consulting machine configuration.
pub(crate) fn build_manifest(workspace: &Path) -> Result<ValidatedWorkspace> {
    compile_workspace(discover_features(workspace)?)
}

/// Lint entry point: the explicitly supplied workspace root itself may be a symlink.
pub(crate) fn build_manifest_following_root(workspace: &Path) -> Result<ValidatedWorkspace> {
    compile_workspace(discover_features_following_root(workspace)?)
}

fn compile_workspace(feature_directories: Vec<FeatureDirectory>) -> Result<ValidatedWorkspace> {
    if feature_directories
        .iter()
        .any(|feature| feature.name == ".dof")
    {
        bail!("feature '.dof' is forbidden");
    }

    let mut claims = Vec::new();
    let mut apply_scripts = Vec::new();
    for feature in feature_directories {
        if let Some(path) = validate_apply_script(&feature)? {
            apply_scripts.push(ApplyScript {
                feature: feature.name.clone(),
                path,
            });
        }
        scan_feature_home(&feature, &mut claims)?;
        read_feature_snippets(&feature, &mut claims)?;
        claims.extend(
            drop_ins::discover(&feature)?
                .into_iter()
                .map(|declaration| Claim {
                    path: declaration.target,
                    feature: feature.name.clone(),
                    kind: ClaimKind::DropIn {
                        fragment: declaration.fragment,
                    },
                }),
        );
    }

    Ok(ValidatedWorkspace {
        targets: compile_claims(claims)?,
        apply_scripts,
    })
}

fn read_feature_snippets(feature: &FeatureDirectory, claims: &mut Vec<Claim>) -> Result<()> {
    let path = feature.path.join("snippets.yaml");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect snippets file for feature '{}' at {}",
                    feature.name,
                    path.display()
                )
            });
        }
    };
    if !metadata.file_type().is_file() {
        bail!(
            "snippets file for feature '{}' at {} is not a regular file",
            feature.name,
            path.display()
        );
    }

    let yaml = fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read snippets file for feature '{}' at {}",
            feature.name,
            path.display()
        )
    })?;
    let snippets = parse_snippets_document(&yaml, &feature.name).with_context(|| {
        format!(
            "failed to parse snippets file for feature '{}' at {}",
            feature.name,
            path.display()
        )
    })?;
    claims.extend(snippets);
    Ok(())
}

fn parse_snippets_document(yaml: &str, feature: &str) -> Result<Vec<Claim>> {
    let document: Value = serde_yaml_ng::from_str(yaml).context("invalid snippets YAML")?;
    let Value::Mapping(root) = document else {
        bail!("snippets file root must be a YAML object");
    };
    let snippets = root
        .get(Value::String("snippets".to_owned()))
        .context("snippets file must contain a top-level 'snippets' item")?;
    let Value::Mapping(snippets) = snippets else {
        bail!("top-level 'snippets' item must be a YAML object");
    };

    let mut claims = Vec::with_capacity(snippets.len());
    for (target, strings) in snippets {
        let Value::String(target) = target else {
            bail!("snippet target keys must be YAML strings");
        };
        let Value::Sequence(strings) = strings else {
            bail!("snippet target '{target}' must contain an array of strings");
        };
        let strings = strings
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                _ => bail!("snippet target '{target}' array items must be YAML strings"),
            })
            .collect::<Result<Vec<_>>>()?;
        claims.push(Claim {
            path: validate_snippet_target(feature, target)?,
            feature: feature.to_owned(),
            kind: ClaimKind::Snippet { strings },
        });
    }
    claims.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(claims)
}

fn validate_snippet_target(feature: &str, target: &str) -> Result<HomePath> {
    if target.chars().any(char::is_control) {
        bail!(
            "snippet target '{}' in feature '{}' must be a safe HOME-relative file path",
            target,
            feature
        );
    }
    HomePath::new(target).map_err(|_| {
        anyhow!(
            "snippet target '{}' in feature '{}' must be a safe HOME-relative file path and may not manage dof state",
            target,
            feature
        )
    })
}

fn validate_apply_script(feature: &FeatureDirectory) -> Result<Option<PathBuf>> {
    let script = feature.path.join("apply");
    match fs::symlink_metadata(&script) {
        Ok(_) => {
            validate_apply_script_path(&feature.name, &script)?;
            Ok(Some(script))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect apply script for feature '{}' at {}",
                feature.name,
                script.display()
            )
        }),
    }
}

pub(crate) fn validate_apply_script_path(feature_name: &str, script: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(script).with_context(|| {
        format!(
            "failed to inspect apply script for feature '{}' at {}",
            feature_name,
            script.display()
        )
    })?;

    if !metadata.file_type().is_file() {
        bail!(
            "apply script for feature '{}' at {} is not a regular file",
            feature_name,
            script.display()
        );
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "apply script for feature '{}' at {} is not executable",
            feature_name,
            script.display()
        );
    }

    let mut file = File::open(script).with_context(|| {
        format!(
            "failed to open apply script for feature '{}' at {}",
            feature_name,
            script.display()
        )
    })?;
    let mut prefix = [0_u8; 2];
    if file.read_exact(&mut prefix).is_err() || prefix != *b"#!" {
        bail!(
            "apply script for feature '{}' at {} must begin with a shebang",
            feature_name,
            script.display()
        );
    }

    Ok(())
}

fn scan_feature_home(feature: &FeatureDirectory, claims: &mut Vec<Claim>) -> Result<()> {
    let home = feature.path.join("home");
    let metadata = match fs::symlink_metadata(&home) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect home directory for feature '{}' at {}",
                    feature.name,
                    home.display()
                )
            });
        }
    };

    if !metadata.file_type().is_dir() {
        bail!(
            "home path for feature '{}' at {} is not a real directory",
            feature.name,
            home.display()
        );
    }

    match fs::symlink_metadata(home.join(".dof")) {
        Ok(_) => bail!(
            "feature '{}' contains forbidden home payload '.dof'",
            feature.name
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect protected home path for feature '{}'",
                    feature.name
                )
            });
        }
    }

    scan_home_directory(feature, &home, Path::new(""), claims)
}

fn scan_home_directory(
    feature: &FeatureDirectory,
    directory: &Path,
    relative_directory: &Path,
    claims: &mut Vec<Claim>,
) -> Result<()> {
    let mut children = fs::read_dir(directory)
        .with_context(|| {
            format!(
                "failed to read home directory for feature '{}' at {}",
                feature.name,
                directory.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read entries in {}", directory.display()))?;
    children.sort_by_key(|entry| entry.file_name());

    for child in children {
        let source = child.path();
        let relative_path = relative_directory.join(child.file_name());
        let file_type = child.file_type().with_context(|| {
            format!(
                "failed to inspect home entry {} for feature '{}'",
                source.display(),
                feature.name
            )
        })?;
        let path = HomePath::new(relative_path.clone()).with_context(|| {
            format!(
                "home entry '{}' in feature '{}' is not a safe HOME-relative path",
                relative_path.display(),
                feature.name
            )
        })?;

        let kind = if file_type.is_dir() {
            ClaimKind::Directory
        } else if file_type.is_file() {
            ClaimKind::CopyFile {
                source: source.clone(),
            }
        } else if file_type.is_symlink() {
            bail!(
                "home entry '{}' in feature '{}' is a symlink; source symlinks are not supported",
                relative_path.display(),
                feature.name
            );
        } else {
            bail!(
                "home entry '{}' in feature '{}' has an unsupported file type",
                relative_path.display(),
                feature.name
            );
        };
        let is_directory = matches!(kind, ClaimKind::Directory);
        claims.push(Claim {
            path,
            feature: feature.name.clone(),
            kind,
        });

        if is_directory {
            scan_home_directory(feature, &source, &relative_path, claims)?;
        }
    }
    Ok(())
}

/// Aggregates exact paths first, then validates parent/descendant structure.
///
/// Sorting by path, claim kind, and owner makes both results and diagnostics
/// independent of filesystem enumeration or feature insertion order.
fn compile_claims(mut claims: Vec<Claim>) -> Result<TargetIndex> {
    claims.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| claim_rank(&left.kind).cmp(&claim_rank(&right.kind)))
            .then_with(|| left.feature.cmp(&right.feature))
            .then_with(|| claim_source(&left.kind).cmp(&claim_source(&right.kind)))
    });

    validate_ascii_case_collisions(&claims)?;

    let mut grouped: BTreeMap<HomePath, Vec<Claim>> = BTreeMap::new();
    for claim in claims {
        grouped.entry(claim.path.clone()).or_default().push(claim);
    }

    let mut targets = BTreeMap::new();
    for (path, claims) in grouped {
        targets.insert(path.clone(), aggregate_exact_claims(&path, claims)?);
    }
    validate_target_structure(&targets)?;
    Ok(TargetIndex(targets))
}

fn claim_rank(kind: &ClaimKind) -> u8 {
    match kind {
        ClaimKind::Directory => 0,
        ClaimKind::CopyFile { .. } => 1,
        ClaimKind::Snippet { .. } => 2,
        ClaimKind::DropIn { .. } => 3,
    }
}

fn claim_source(kind: &ClaimKind) -> Option<&Path> {
    match kind {
        ClaimKind::CopyFile { source } => Some(source),
        ClaimKind::DropIn { fragment } => Some(&fragment.source),
        ClaimKind::Directory | ClaimKind::Snippet { .. } => None,
    }
}

fn validate_ascii_case_collisions(claims: &[Claim]) -> Result<()> {
    let mut paths: BTreeMap<Vec<u8>, Vec<(HomePath, &Claim)>> = BTreeMap::new();
    for claim in claims {
        for path in claim
            .path
            .parents()
            .chain(std::iter::once(claim.path.clone()))
        {
            let folded = path
                .as_path()
                .as_os_str()
                .as_bytes()
                .iter()
                .map(u8::to_ascii_lowercase)
                .collect::<Vec<_>>();
            paths.entry(folded).or_default().push((path, claim));
        }
    }

    for records in paths.values_mut() {
        records.sort_by(|(left_path, left_claim), (right_path, right_claim)| {
            left_path
                .cmp(right_path)
                .then_with(|| left_claim.feature.cmp(&right_claim.feature))
                .then_with(|| claim_source(&left_claim.kind).cmp(&claim_source(&right_claim.kind)))
        });
        for (index, (left_path, left_claim)) in records.iter().enumerate() {
            for (right_path, right_claim) in &records[index + 1..] {
                if left_path == right_path
                    || (!claim_is_drop_in(left_claim) && !claim_is_drop_in(right_claim))
                {
                    continue;
                }
                bail!(
                    "drop-in portability collision: path '{}' ({}) differs only by ASCII case from path '{}' ({})",
                    left_path,
                    describe_claim(left_claim),
                    right_path,
                    describe_claim(right_claim)
                );
            }
        }
    }
    Ok(())
}

fn claim_is_drop_in(claim: &Claim) -> bool {
    matches!(claim.kind, ClaimKind::DropIn { .. })
}

fn describe_claim(claim: &Claim) -> String {
    match &claim.kind {
        ClaimKind::Directory => format!("directory from feature '{}'", claim.feature),
        ClaimKind::CopyFile { source } => format!(
            "copied by feature '{}' from {}",
            claim.feature,
            source.display()
        ),
        ClaimKind::Snippet { .. } => {
            format!("snippet target from feature '{}'", claim.feature)
        }
        ClaimKind::DropIn { fragment } => format!(
            "drop-in fragment '{}' from feature '{}' at {}",
            fragment.filename,
            fragment.feature,
            fragment.source.display()
        ),
    }
}

fn aggregate_exact_claims(path: &HomePath, claims: Vec<Claim>) -> Result<Target> {
    let mut directories = Vec::new();
    let mut copies = Vec::new();
    let mut snippets = Vec::new();
    let mut drop_ins = Vec::new();
    for claim in claims {
        match claim.kind {
            ClaimKind::Directory => directories.push(claim.feature),
            ClaimKind::CopyFile { source } => copies.push((claim.feature, source)),
            ClaimKind::Snippet { strings } => snippets.push((claim.feature, strings)),
            ClaimKind::DropIn { fragment } => drop_ins.push(fragment),
        }
    }
    directories.sort();
    copies.sort_by(|left, right| left.0.cmp(&right.0));
    snippets.sort_by(|left, right| left.0.cmp(&right.0));
    drop_ins.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.filename.cmp(&right.filename))
            .then_with(|| left.feature.cmp(&right.feature))
            .then_with(|| left.source.cmp(&right.source))
    });

    if let (Some((file_owner, _)), Some(directory_owner)) = (copies.first(), directories.first()) {
        bail!(
            "destination '{}' is a file in feature '{}' and a directory in feature '{}'",
            path,
            file_owner,
            directory_owner
        );
    }
    if let (Some((snippet_owner, _)), Some(directory_owner)) =
        (snippets.first(), directories.first())
    {
        bail!(
            "destination '{}' is a snippet target in feature '{}' and a directory in feature '{}'",
            path,
            snippet_owner,
            directory_owner
        );
    }
    if let (Some((file_owner, _)), Some((snippet_owner, _))) = (copies.first(), snippets.first()) {
        bail!(
            "destination '{}' is copied by feature '{}' and managed by snippets in feature '{}'",
            path,
            file_owner,
            snippet_owner
        );
    }
    if let (Some(_), Some(directory_owner)) = (drop_ins.first(), directories.first()) {
        bail!(
            "drop-in target '{}' ({}) is also a directory in feature '{}'",
            path,
            describe_drop_in_fragments(&drop_ins),
            directory_owner
        );
    }
    if let (Some(_), Some((file_owner, source))) = (drop_ins.first(), copies.first()) {
        bail!(
            "drop-in target '{}' ({}) is also copied by feature '{}' from {}",
            path,
            describe_drop_in_fragments(&drop_ins),
            file_owner,
            source.display()
        );
    }
    if let (Some(_), Some((snippet_owner, _))) = (drop_ins.first(), snippets.first()) {
        bail!(
            "drop-in target '{}' ({}) is also managed by snippets in feature '{}'",
            path,
            describe_drop_in_fragments(&drop_ins),
            snippet_owner
        );
    }

    if copies.len() > 1 {
        bail!(
            "destination '{}' is a file in both feature '{}' and feature '{}'",
            path,
            copies[0].0,
            copies[1].0
        );
    }
    validate_drop_in_reservations(path, &drop_ins)?;

    if let Some((feature, source)) = copies.into_iter().next() {
        return Ok(Target::CopyFile { feature, source });
    }
    if !snippets.is_empty() {
        return Ok(Target::Snippets {
            contributions: snippets.into_iter().collect(),
        });
    }
    if !drop_ins.is_empty() {
        return Ok(Target::DropIns {
            fragments: drop_ins,
        });
    }
    Ok(Target::Directory {
        features: directories.into_iter().collect(),
    })
}

fn validate_drop_in_reservations(path: &HomePath, fragments: &[DropInFragment]) -> Result<()> {
    let mut filenames = BTreeMap::new();
    for fragment in fragments {
        if let Some(previous) = filenames.insert(fragment.filename.as_str(), fragment) {
            bail!(
                "drop-in target '{}' fragment filename '{}' is declared by feature '{}' at {} and feature '{}' at {}",
                path,
                fragment.filename,
                previous.feature,
                previous.source.display(),
                fragment.feature,
                fragment.source.display()
            );
        }
    }

    let mut orders = BTreeMap::new();
    for fragment in fragments {
        if let Some(previous) = orders.insert(fragment.order, fragment) {
            bail!(
                "drop-in target '{}' order {:02} is declared by fragment '{}' in feature '{}' at {} and fragment '{}' in feature '{}' at {}",
                path,
                fragment.order,
                previous.filename,
                previous.feature,
                previous.source.display(),
                fragment.filename,
                fragment.feature,
                fragment.source.display()
            );
        }
    }
    Ok(())
}

fn describe_drop_in_fragments(fragments: &[DropInFragment]) -> String {
    fragments
        .iter()
        .map(|fragment| {
            format!(
                "fragment '{}' from feature '{}' at {}",
                fragment.filename,
                fragment.feature,
                fragment.source.display()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn validate_target_structure(targets: &BTreeMap<HomePath, Target>) -> Result<()> {
    for (path, target) in targets {
        for ancestor in path.as_path().ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            let ancestor = HomePath(ancestor.to_path_buf());
            let Some(ancestor_target) = targets.get(&ancestor) else {
                continue;
            };
            if target_is_file(ancestor_target) {
                bail_structural_conflict(path, target, &ancestor, ancestor_target)?;
            }
        }
    }
    Ok(())
}

fn target_is_file(target: &Target) -> bool {
    matches!(
        target,
        Target::CopyFile { .. } | Target::Snippets { .. } | Target::DropIns { .. }
    )
}

fn bail_structural_conflict(
    path: &HomePath,
    target: &Target,
    ancestor: &HomePath,
    ancestor_target: &Target,
) -> Result<()> {
    if matches!(target, Target::DropIns { .. }) || matches!(ancestor_target, Target::DropIns { .. })
    {
        bail!(
            "destination '{}' ({}) conflicts structurally with ancestor '{}' ({})",
            path,
            describe_target(target),
            ancestor,
            describe_target(ancestor_target)
        );
    }

    let owner = target_owner(target);
    let ancestor_owner = target_owner(ancestor_target);
    match (target, ancestor_target) {
        (Target::Snippets { .. }, Target::CopyFile { .. }) => bail!(
            "snippet target '{}' in feature '{}' is nested beneath copied file '{}' from feature '{}'",
            path,
            owner,
            ancestor,
            ancestor_owner
        ),
        (Target::Snippets { .. }, Target::Snippets { .. }) => bail!(
            "snippet target '{}' in feature '{}' is nested beneath snippet target '{}' from feature '{}'",
            path,
            owner,
            ancestor,
            ancestor_owner
        ),
        (_, Target::Snippets { .. }) => bail!(
            "destination '{}' in feature '{}' is nested beneath snippet target '{}' from feature '{}'",
            path,
            owner,
            ancestor,
            ancestor_owner
        ),
        (_, Target::CopyFile { .. }) => bail!(
            "destination '{}' in feature '{}' is nested beneath file destination '{}' from feature '{}'",
            path,
            owner,
            ancestor,
            ancestor_owner
        ),
        (_, Target::DropIns { .. }) => bail!(
            "destination '{}' in feature '{}' is nested beneath drop-in target '{}' from feature '{}'",
            path,
            owner,
            ancestor,
            ancestor_owner
        ),
        (_, Target::Directory { .. }) => unreachable!("directories are not structural blockers"),
    }
}

fn describe_target(target: &Target) -> String {
    match target {
        Target::Directory { features } => {
            format!(
                "directory from feature(s) {}",
                join_features(features.iter())
            )
        }
        Target::CopyFile { feature, source } => {
            format!("copied by feature '{feature}' from {}", source.display())
        }
        Target::Snippets { contributions } => format!(
            "snippet target from feature(s) {}",
            join_features(contributions.keys())
        ),
        Target::DropIns { fragments } => describe_drop_in_fragments(fragments),
    }
}

fn join_features<'a>(features: impl Iterator<Item = &'a String>) -> String {
    features
        .map(|feature| format!("'{feature}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn target_owner(target: &Target) -> &str {
    match target {
        Target::Directory { features } => features
            .iter()
            .next()
            .expect("compiled directory target has an owner"),
        Target::CopyFile { feature, .. } => feature,
        Target::Snippets { contributions } => contributions
            .keys()
            .next()
            .expect("compiled snippet target has an owner"),
        Target::DropIns { fragments } => {
            &fragments
                .first()
                .expect("compiled drop-in target has an owner")
                .feature
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn home_path_accepts_safe_paths_and_preserves_non_utf8_bytes() {
        for path in [PathBuf::from(".bashrc"), PathBuf::from(".config/app")] {
            assert_eq!(HomePath::new(path.clone()).unwrap().as_path(), path);
        }

        let path = PathBuf::from(OsString::from_vec(vec![b'f', 0x80]));
        let validated = HomePath::new(path.clone()).unwrap();
        assert_eq!(
            validated.as_path().as_os_str().as_encoded_bytes(),
            path.as_os_str().as_encoded_bytes()
        );
    }

    #[test]
    fn home_path_normalizes_filesystem_aliases() {
        for path in ["foo/", "foo/.", "foo//bar", "foo/./bar"] {
            let expected = if path.starts_with("foo/") && path.contains("bar") {
                Path::new("foo/bar")
            } else {
                Path::new("foo")
            };
            assert_eq!(HomePath::new(path).unwrap().as_path(), expected, "{path:?}");
        }
    }

    #[test]
    fn home_path_rejects_unsafe_and_protected_paths() {
        for path in [
            "",
            ".",
            "/tmp/target",
            "../target",
            "./target",
            "parent/../target",
            ".dof",
            ".dof/config.yaml",
        ] {
            assert!(HomePath::new(path).is_err(), "{path:?} unexpectedly passed");
        }
    }

    #[test]
    fn home_path_parents_are_shallow_to_deep() {
        let path = HomePath::new(".config/app/settings.yaml").unwrap();
        let parents = path
            .parents()
            .map(|parent| parent.as_path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            parents,
            [PathBuf::from(".config"), PathBuf::from(".config/app")]
        );
    }

    #[test]
    fn exact_claim_compatibility_matrix_is_order_independent() {
        let cases = [
            (TestKind::Directory, TestKind::Directory, true),
            (TestKind::Directory, TestKind::Copy, false),
            (TestKind::Directory, TestKind::Snippet, false),
            (TestKind::Directory, TestKind::DropIn, false),
            (TestKind::Copy, TestKind::Copy, false),
            (TestKind::Copy, TestKind::Snippet, false),
            (TestKind::Copy, TestKind::DropIn, false),
            (TestKind::Snippet, TestKind::Snippet, true),
            (TestKind::Snippet, TestKind::DropIn, false),
            (TestKind::DropIn, TestKind::DropIn, true),
        ];

        for (left, right, valid) in cases {
            let claims = vec![
                test_claim("zeta", left, "target"),
                test_claim("alpha", right, "target"),
            ];
            let forward = compile_claims(claims.clone());
            let reverse = compile_claims(claims.into_iter().rev().collect());
            assert_eq!(forward.is_ok(), valid, "{left:?} + {right:?}");
            assert_eq!(reverse.is_ok(), valid, "{right:?} + {left:?}");
            match (forward, reverse) {
                (Ok(forward), Ok(reverse)) => {
                    assert_eq!(target_signature(forward), target_signature(reverse));
                }
                (Err(forward), Err(reverse)) => {
                    assert_eq!(format!("{forward:#}"), format!("{reverse:#}"));
                }
                _ => panic!("claim order changed compilation result"),
            }
        }
    }

    #[test]
    fn ancestor_conflicts_are_insertion_order_independent() {
        let cases = [
            (TestKind::Copy, TestKind::Copy),
            (TestKind::Copy, TestKind::Snippet),
            (TestKind::Copy, TestKind::DropIn),
            (TestKind::Snippet, TestKind::Copy),
            (TestKind::Snippet, TestKind::Snippet),
            (TestKind::Snippet, TestKind::DropIn),
            (TestKind::DropIn, TestKind::Copy),
            (TestKind::DropIn, TestKind::Snippet),
            (TestKind::DropIn, TestKind::DropIn),
        ];

        for (ancestor, descendant) in cases {
            let claims = vec![
                test_claim("zeta", descendant, ".config/tool/settings"),
                test_claim("alpha", ancestor, ".config"),
            ];
            let forward = compile_claims(claims.clone()).unwrap_err();
            let reverse = compile_claims(claims.into_iter().rev().collect()).unwrap_err();
            assert_eq!(format!("{forward:#}"), format!("{reverse:#}"));
            assert!(format!("{forward:#}").contains(".config/tool/settings"));
        }
    }

    #[test]
    fn compilation_is_stable_across_all_claim_permutations() {
        let claims = vec![
            test_claim("beta", TestKind::Directory, ".config"),
            test_claim("alpha", TestKind::Directory, ".config"),
            test_claim("gamma", TestKind::Snippet, ".profile"),
            test_claim("alpha", TestKind::DropIn, ".Brewfile"),
            test_claim("zeta", TestKind::DropIn, ".Brewfile"),
        ];
        let expected = target_signature(compile_claims(claims.clone()).unwrap());
        for permutation in permutations(claims) {
            assert_eq!(
                target_signature(compile_claims(permutation).unwrap()),
                expected
            );
        }
    }

    #[test]
    fn drop_in_case_collisions_cover_implied_parents_only_when_involved() {
        let claims = vec![
            test_claim("drop", TestKind::DropIn, ".Config/tool.conf"),
            test_claim("snippet", TestKind::Snippet, ".config/other.conf"),
        ];
        let forward = compile_claims(claims.clone()).unwrap_err();
        let reverse = compile_claims(claims.into_iter().rev().collect()).unwrap_err();
        assert_eq!(format!("{forward:#}"), format!("{reverse:#}"));
        assert!(format!("{forward:#}").contains("ASCII case"));

        compile_claims(vec![
            test_claim("copy", TestKind::Copy, ".Config/tool.conf"),
            test_claim("snippet", TestKind::Snippet, ".config/other.conf"),
        ])
        .expect("case-only aliases remain unchanged when no drop-in participates");
    }

    #[test]
    fn drop_in_reservations_reject_duplicate_orders_and_filenames() {
        let same_order = vec![
            test_drop_in_claim("alpha", ".Brewfile", 10, "10-alpha"),
            test_drop_in_claim("zeta", ".Brewfile", 10, "10-zeta"),
        ];
        assert!(format!("{:#}", compile_claims(same_order).unwrap_err()).contains("order 10"));

        let same_filename = vec![
            test_drop_in_claim("alpha", ".Brewfile", 10, "10-shared"),
            test_drop_in_claim("zeta", ".Brewfile", 10, "10-shared"),
        ];
        assert!(format!("{:#}", compile_claims(same_filename).unwrap_err()).contains("filename"));

        compile_claims(vec![
            test_drop_in_claim("alpha", ".Brewfile", 10, "10-shared"),
            test_drop_in_claim("zeta", ".profile", 10, "10-shared"),
        ])
        .expect("orders and filenames are reserved independently for each target");
    }

    #[test]
    fn manifest_includes_sorted_files_and_empty_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        fs::create_dir_all(workspace.join("features/default/home/z-empty")).unwrap();
        fs::create_dir_all(workspace.join("features/default/home/.git")).unwrap();
        fs::write(workspace.join("features/default/home/a-file"), "a\n").unwrap();
        fs::write(workspace.join("features/default/home/.git/config"), "git\n").unwrap();

        let (targets, scripts) = build_manifest(&workspace).unwrap().into_parts();
        assert!(scripts.is_empty());
        let actual = targets
            .into_iter()
            .map(|(path, target)| {
                let kind = match target {
                    Target::Directory { .. } => "directory",
                    Target::CopyFile { source, .. } => {
                        assert!(source.is_absolute());
                        "file"
                    }
                    Target::Snippets { .. } => "snippets",
                    Target::DropIns { .. } => "drop-ins",
                };
                (path.as_path().to_owned(), kind)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                (PathBuf::from(".git"), "directory"),
                (PathBuf::from(".git/config"), "file"),
                (PathBuf::from("a-file"), "file"),
                (PathBuf::from("z-empty"), "directory"),
            ]
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum TestKind {
        Directory,
        Copy,
        Snippet,
        DropIn,
    }

    fn test_claim(feature: &str, kind: TestKind, path: &str) -> Claim {
        let kind = match kind {
            TestKind::Directory => ClaimKind::Directory,
            TestKind::Copy => ClaimKind::CopyFile {
                source: PathBuf::from(format!("/source/{feature}")),
            },
            TestKind::Snippet => ClaimKind::Snippet {
                strings: vec![feature.to_owned()],
            },
            TestKind::DropIn => {
                let order = if feature == "alpha" { 10 } else { 20 };
                return test_drop_in_claim(feature, path, order, &format!("{order:02}-{feature}"));
            }
        };
        Claim {
            path: HomePath::new(path).unwrap(),
            feature: feature.to_owned(),
            kind,
        }
    }

    fn test_drop_in_claim(feature: &str, path: &str, order: u8, filename: &str) -> Claim {
        Claim {
            path: HomePath::new(path).unwrap(),
            feature: feature.to_owned(),
            kind: ClaimKind::DropIn {
                fragment: DropInFragment {
                    feature: feature.to_owned(),
                    order,
                    contents: format!("{feature}\n").into_bytes(),
                    filename: filename.to_owned(),
                    source: PathBuf::from(format!("/source/{feature}/{filename}")),
                },
            },
        }
    }

    fn target_signature(index: TargetIndex) -> Vec<(PathBuf, String)> {
        index
            .into_iter()
            .map(|(path, target)| {
                let value = match target {
                    Target::Directory { features } => {
                        format!("directory:{features:?}")
                    }
                    Target::CopyFile { feature, source } => {
                        format!("copy:{feature}:{}", source.display())
                    }
                    Target::Snippets { contributions } => {
                        format!("snippets:{contributions:?}")
                    }
                    Target::DropIns { fragments } => format!(
                        "drop-ins:{:?}",
                        fragments
                            .into_iter()
                            .map(|fragment| (
                                fragment.feature,
                                fragment.order,
                                fragment.filename,
                                fragment.contents,
                            ))
                            .collect::<Vec<_>>()
                    ),
                };
                (path.as_path().to_owned(), value)
            })
            .collect()
    }

    fn permutations<T: Clone>(items: Vec<T>) -> Vec<Vec<T>> {
        fn visit<T: Clone>(remaining: Vec<T>, prefix: Vec<T>, output: &mut Vec<Vec<T>>) {
            if remaining.is_empty() {
                output.push(prefix);
                return;
            }
            for index in 0..remaining.len() {
                let mut next_remaining = remaining.clone();
                let item = next_remaining.remove(index);
                let mut next_prefix = prefix.clone();
                next_prefix.push(item);
                visit(next_remaining, next_prefix, output);
            }
        }

        let mut output = Vec::new();
        visit(items, Vec::new(), &mut output);
        output
    }
}
