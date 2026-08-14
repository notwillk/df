use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::workspace::{FeatureDirectory, HomePath};

#[derive(Clone, Debug)]
pub(crate) struct DropInFragment {
    pub(crate) feature: String,
    pub(crate) order: u8,
    pub(crate) contents: Vec<u8>,
    pub(crate) filename: String,
    pub(crate) source: PathBuf,
}

#[derive(Debug)]
pub(crate) struct DropInDeclaration {
    pub(crate) target: HomePath,
    pub(crate) fragment: DropInFragment,
}

/// Discovers and validates one feature's `drop-ins/` tree.
///
/// Directories containing files are compilation units and must end in `.d`.
/// Directories containing directories are containers. Mixing the two shapes,
/// using unsupported source types, or leaving a nested directory empty is an
/// error. The top-level `drop-ins/` directory itself may be empty.
pub(crate) fn discover(feature: &FeatureDirectory) -> Result<Vec<DropInDeclaration>> {
    let root = feature.path.join("drop-ins");
    let metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect drop-ins directory for feature '{}' at {}",
                    feature.name,
                    root.display()
                )
            });
        }
    };
    if !metadata.file_type().is_dir() {
        bail!(
            "drop-ins path for feature '{}' at {} is not a real directory",
            feature.name,
            root.display()
        );
    }

    let mut declarations = Vec::new();
    scan_directory(feature, &root, Path::new(""), true, &mut declarations)?;
    declarations.sort_by(|left, right| {
        left.target
            .cmp(&right.target)
            .then_with(|| left.fragment.order.cmp(&right.fragment.order))
            .then_with(|| left.fragment.filename.cmp(&right.fragment.filename))
            .then_with(|| left.fragment.feature.cmp(&right.fragment.feature))
            .then_with(|| left.fragment.source.cmp(&right.fragment.source))
    });
    Ok(declarations)
}

fn scan_directory(
    feature: &FeatureDirectory,
    directory: &Path,
    relative_directory: &Path,
    is_root: bool,
    declarations: &mut Vec<DropInDeclaration>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| {
            format!(
                "failed to read drop-ins directory for feature '{}' at {}",
                feature.name,
                directory.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read entries in {}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    if entries.is_empty() {
        if is_root {
            return Ok(());
        }
        bail!(
            "drop-ins directory '{}' in feature '{}' is empty; only the top-level drop-ins directory may be empty",
            relative_directory.display(),
            feature.name
        );
    }

    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in entries {
        let source = entry.path();
        let file_type = entry.file_type().with_context(|| {
            format!(
                "failed to inspect drop-in entry {} for feature '{}'",
                source.display(),
                feature.name
            )
        })?;
        if file_type.is_dir() {
            let name = entry.file_name().into_string().map_err(|name| {
                anyhow::anyhow!(
                    "drop-ins directory component in feature '{}' is not valid UTF-8: {}",
                    feature.name,
                    Path::new(&name).display()
                )
            })?;
            directories.push((name, source));
        } else if file_type.is_file() {
            files.push((entry.file_name(), source));
        } else if file_type.is_symlink() {
            bail!(
                "drop-in entry '{}' in feature '{}' is a symlink; source symlinks are not supported",
                relative_directory.join(entry.file_name()).display(),
                feature.name
            );
        } else {
            bail!(
                "drop-in entry '{}' in feature '{}' has an unsupported file type",
                relative_directory.join(entry.file_name()).display(),
                feature.name
            );
        }
    }

    if !directories.is_empty() && !files.is_empty() {
        bail!(
            "drop-ins directory '{}' in feature '{}' mixes fragment files and child directories",
            display_relative_root(relative_directory),
            feature.name
        );
    }

    if !files.is_empty() {
        if is_root {
            bail!(
                "drop-in fragment '{}' in feature '{}' is directly beneath the drop-ins root; fragments must be inside a terminal '.d' directory",
                Path::new(&files[0].0).display(),
                feature.name
            );
        }
        let target = target_for_terminal_directory(feature, relative_directory)?;
        for (filename, source) in files {
            declarations.push(DropInDeclaration {
                target: target.clone(),
                fragment: read_fragment(feature, filename, source)?,
            });
        }
        return Ok(());
    }

    for (name, child) in directories {
        scan_directory(
            feature,
            &child,
            &relative_directory.join(name),
            false,
            declarations,
        )?;
    }
    Ok(())
}

fn target_for_terminal_directory(
    feature: &FeatureDirectory,
    relative_directory: &Path,
) -> Result<HomePath> {
    let Some(filename) = relative_directory.file_name() else {
        bail!(
            "files directly beneath the drop-ins root in feature '{}' are not valid fragments",
            feature.name
        );
    };
    let filename = filename.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "drop-ins directory component in feature '{}' is not valid UTF-8: {}",
            feature.name,
            relative_directory.display()
        )
    })?;
    let Some(target_filename) = filename.strip_suffix(".d") else {
        bail!(
            "drop-ins directory '{}' in feature '{}' contains fragments but does not end in '.d'",
            relative_directory.display(),
            feature.name
        );
    };
    let mut target_components = Path::new(target_filename).components();
    if !matches!(target_components.next(), Some(Component::Normal(_)))
        || target_components.next().is_some()
    {
        bail!(
            "terminal drop-ins directory '{}' in feature '{}' must name one normal target component before '.d'",
            relative_directory.display(),
            feature.name
        );
    }

    let mut target = relative_directory
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    target.push(target_filename);
    let target = HomePath::new(&target).with_context(|| {
        format!(
            "drop-in target '{}' in feature '{}' is not a safe HOME-relative file path",
            target.display(),
            feature.name
        )
    })?;
    if target
        .as_path()
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .is_some_and(|component| component.eq_ignore_ascii_case(".dof"))
    {
        bail!(
            "drop-in target '{}' in feature '{}' may not manage dof state",
            target,
            feature.name
        );
    }
    Ok(target)
}

fn read_fragment(
    feature: &FeatureDirectory,
    filename: std::ffi::OsString,
    source: PathBuf,
) -> Result<DropInFragment> {
    let filename = filename.into_string().map_err(|filename| {
        anyhow::anyhow!(
            "drop-in fragment name in feature '{}' is not valid UTF-8: {}",
            feature.name,
            Path::new(&filename).display()
        )
    })?;
    let order = parse_fragment_name(&filename).with_context(|| {
        format!(
            "invalid drop-in fragment '{}' in feature '{}' at {}",
            filename,
            feature.name,
            source.display()
        )
    })?;
    let contents = fs::read(&source).with_context(|| {
        format!(
            "failed to read drop-in fragment '{}' in feature '{}' at {}",
            filename,
            feature.name,
            source.display()
        )
    })?;
    validate_fragment_contents(&contents).with_context(|| {
        format!(
            "invalid drop-in fragment '{}' in feature '{}' at {}",
            filename,
            feature.name,
            source.display()
        )
    })?;

    Ok(DropInFragment {
        feature: feature.name.clone(),
        order,
        contents,
        filename,
        source,
    })
}

fn parse_fragment_name(filename: &str) -> Result<u8> {
    let bytes = filename.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_digit()
        || !bytes[1].is_ascii_digit()
        || bytes[2] != b'-'
        || !matches!(bytes[3], b'a'..=b'z' | b'0'..=b'9')
        || !bytes[4..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        bail!("fragment name must match ^[0-9]{{2}}-[a-z0-9][a-z0-9._-]*$");
    }
    Ok((bytes[0] - b'0') * 10 + (bytes[1] - b'0'))
}

fn validate_fragment_contents(contents: &[u8]) -> Result<()> {
    if contents.is_empty() {
        bail!("fragment must not be empty");
    }
    std::str::from_utf8(contents).context("fragment must contain valid UTF-8")?;
    if contents.contains(&0) {
        bail!("fragment must not contain NUL bytes");
    }
    if !contents.ends_with(b"\n") {
        bail!("fragment must end with a newline");
    }
    Ok(())
}

fn display_relative_root(path: &Path) -> std::path::Display<'_> {
    if path.as_os_str().is_empty() {
        Path::new(".").display()
    } else {
        path.display()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn fixture() -> (tempfile::TempDir, FeatureDirectory) {
        let fixture = tempfile::tempdir().unwrap();
        let feature = FeatureDirectory {
            name: "default".to_owned(),
            path: fixture.path().join("features/default"),
        };
        fs::create_dir_all(&feature.path).unwrap();
        (fixture, feature)
    }

    #[test]
    fn fragment_name_grammar_is_exact() {
        for (name, order) in [
            ("00-a", 0),
            ("09-shell.profile", 9),
            ("42-a_b-c.d", 42),
            ("99-9", 99),
        ] {
            assert_eq!(parse_fragment_name(name).unwrap(), order, "{name}");
        }
        for name in [
            "0-a", "000-a", "01-", "01-A", "01-a+", "aa-name", "01-a/b", "é1-name",
        ] {
            assert!(
                parse_fragment_name(name).is_err(),
                "{name} unexpectedly passed"
            );
        }
    }

    #[test]
    fn fragment_content_validation_is_strict() {
        for valid in [b"one\n".as_slice(), "multi\nline\n".as_bytes()] {
            validate_fragment_contents(valid).unwrap();
        }
        for invalid in [
            b"".as_slice(),
            b"missing newline".as_slice(),
            b"nul\0byte\n".as_slice(),
            &[0xff, b'\n'],
        ] {
            assert!(
                validate_fragment_contents(invalid).is_err(),
                "{invalid:?} unexpectedly passed"
            );
        }
    }

    #[test]
    fn discovers_nested_targets_and_preserves_nonterminal_dot_d() {
        let (_fixture, feature) = fixture();
        let first = feature.path.join("drop-ins/.Brewfile.d/10-base");
        let nested = feature
            .path
            .join("drop-ins/.config/systemd/user/example.service.d/override.conf.d/20-work");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&first, "tap one\n").unwrap();
        fs::write(&nested, "[Service]\n").unwrap();

        let declarations = discover(&feature).unwrap();
        let actual = declarations
            .iter()
            .map(|declaration| {
                (
                    declaration.target.as_path().to_owned(),
                    declaration.fragment.order,
                    declaration.fragment.contents.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                (PathBuf::from(".Brewfile"), 10, b"tap one\n".to_vec()),
                (
                    PathBuf::from(".config/systemd/user/example.service.d/override.conf"),
                    20,
                    b"[Service]\n".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn missing_and_empty_top_level_directories_are_valid() {
        let (_fixture, feature) = fixture();
        assert!(discover(&feature).unwrap().is_empty());
        fs::create_dir(feature.path.join("drop-ins")).unwrap();
        assert!(discover(&feature).unwrap().is_empty());
    }

    #[test]
    fn top_level_drop_ins_must_be_a_real_directory() {
        let (_fixture, feature) = fixture();
        fs::write(feature.path.join("drop-ins"), "not a directory\n").unwrap();
        assert!(discover(&feature).is_err());
    }

    #[test]
    fn strict_tree_rejects_empty_mixed_or_orphan_shapes() {
        for case in ["empty", "mixed", "orphan"] {
            let (_fixture, feature) = fixture();
            match case {
                "empty" => {
                    fs::create_dir_all(feature.path.join("drop-ins/container")).unwrap();
                }
                "mixed" => {
                    let root = feature.path.join("drop-ins/target.d");
                    fs::create_dir_all(root.join("child")).unwrap();
                    fs::write(root.join("10-base"), "base\n").unwrap();
                }
                "orphan" => {
                    let root = feature.path.join("drop-ins/not-terminal");
                    fs::create_dir_all(&root).unwrap();
                    fs::write(root.join("10-base"), "base\n").unwrap();
                }
                _ => unreachable!(),
            }
            assert!(discover(&feature).is_err(), "{case} unexpectedly passed");
        }
    }

    #[test]
    fn terminal_directory_must_name_a_normal_target_component() {
        for terminal_name in [".d", "..d", "...d"] {
            let (_fixture, feature) = fixture();
            let terminal = feature.path.join("drop-ins/container").join(terminal_name);
            fs::create_dir_all(&terminal).unwrap();
            fs::write(terminal.join("10-base"), "base\n").unwrap();
            assert!(
                discover(&feature).is_err(),
                "{terminal_name} unexpectedly produced a target"
            );
        }
    }

    #[test]
    fn rejects_source_symlinks_and_case_variants_of_dof_state() {
        let (_fixture, feature) = fixture();
        let terminal = feature.path.join("drop-ins/target.d");
        fs::create_dir_all(&terminal).unwrap();
        symlink("missing", terminal.join("10-link")).unwrap();
        assert!(
            discover(&feature)
                .unwrap_err()
                .to_string()
                .contains("symlink")
        );

        fs::remove_file(terminal.join("10-link")).unwrap();
        fs::remove_dir_all(feature.path.join("drop-ins")).unwrap();
        let protected = feature.path.join("drop-ins/.DOF/config.yaml.d");
        fs::create_dir_all(&protected).unwrap();
        fs::write(protected.join("10-base"), "protected\n").unwrap();
        assert!(
            discover(&feature)
                .unwrap_err()
                .to_string()
                .contains("may not manage dof state")
        );
    }
}
