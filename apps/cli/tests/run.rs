use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tempfile::TempDir;

mod support;

use support::{binary, dof, output, set_mode, stderr, stdout, write_executable};

#[test]
fn run_help_describes_arguments_and_requires_a_script_name() {
    let help = output(Command::new(binary()).args(["run", "--help"]));
    assert!(help.status.success(), "{}", stderr(&help));
    let help = stdout(&help);
    assert!(help.contains("Usage: dof run <SCRIPT> [ARGUMENTS]..."));
    assert!(help.contains("Arguments passed to the executable"));

    let missing = output(Command::new(binary()).arg("run"));
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("<SCRIPT>"));
}

#[test]
fn forwards_arguments_streams_environment_and_working_directory() {
    let fixture = Fixture::new();
    fixture.write_script(
        "inspect",
        r#"#!/bin/sh
payload=
while IFS= read -r line || [ -n "$line" ]; do
  if [ -n "$payload" ]; then
    payload="$payload|"
  fi
  payload="$payload$line"
done

printf 'argc=<%s>\n' "$#"
index=1
for argument do
  printf 'arg%s=<%s>\n' "$index" "$argument"
  index=$((index + 1))
done
printf 'stdin=<%s>\n' "$payload"
printf 'environment=<%s>\n' "$DOF_RUN_TEST_ENV"
printf 'cwd=<%s>\n' "$PWD"
printf 'child stderr\n' >&2
"#,
    );
    let working_directory = fixture.root.path().join("working directory");
    fs::create_dir(&working_directory).unwrap();

    let mut child = dof(&fixture.home)
        .current_dir(&working_directory)
        .env("DOF_RUN_TEST_ENV", "inherited value")
        .args([
            "run",
            "inspect",
            "ordinary",
            "--long=value",
            "-x",
            "two words",
            "--help",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first line\nsecond line\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    let canonical_working_directory = working_directory.canonicalize().unwrap();

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        stdout(&result),
        format!(
            concat!(
                "argc=<5>\n",
                "arg1=<ordinary>\n",
                "arg2=<--long=value>\n",
                "arg3=<-x>\n",
                "arg4=<two words>\n",
                "arg5=<--help>\n",
                "stdin=<first line|second line>\n",
                "environment=<inherited value>\n",
                "cwd=<{}>\n",
            ),
            canonical_working_directory.display()
        )
    );
    assert_eq!(stderr(&result), "child stderr\n");
}

#[test]
fn returns_the_childs_exact_exit_status() {
    let fixture = Fixture::new();
    fixture.write_script(
        "fail",
        r#"#!/bin/sh
printf 'before exit\n'
printf 'failure details\n' >&2
exit 37
"#,
    );

    let result = output(dof(&fixture.home).args(["run", "fail"]));

    assert_eq!(result.status.code(), Some(37));
    assert_eq!(stdout(&result), "before exit\n");
    assert_eq!(stderr(&result), "failure details\n");
}

#[test]
fn propagates_child_signals() {
    let fixture = Fixture::new();
    fixture.write_script("terminate", "#!/bin/sh\nkill -TERM $$\n");

    let result = output(dof(&fixture.home).args(["run", "terminate"]));

    assert_eq!(result.status.signal(), Some(15));
}

#[test]
fn relative_home_is_resolved_from_the_callers_working_directory() {
    let fixture = Fixture::new();
    fixture.write_script("where", "#!/bin/sh\nprintf 'relative home works\\n'\n");
    let relative_home = fixture.home.file_name().unwrap();

    let result = output(
        Command::new(binary())
            .current_dir(fixture.root.path())
            .env("HOME", relative_home)
            .args(["run", "where"]),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(stdout(&result), "relative home works\n");
}

#[test]
fn forwards_non_utf8_arguments_without_conversion() {
    let fixture = Fixture::new();
    fixture.write_script("bytes", "#!/bin/sh\nprintf %s \"$1\"\n");
    let argument = OsString::from_vec(vec![b'f', b'o', 0x80, b'o']);

    let result = output(dof(&fixture.home).arg("run").arg("bytes").arg(&argument));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(result.stdout, argument.into_vec());
}

#[test]
fn missing_home_bin_or_script_fail_clearly() {
    for home in [None, Some("")] {
        let mut command = Command::new(binary());
        command.arg("run").arg("missing");
        match home {
            Some(home) => {
                command.env("HOME", home);
            }
            None => {
                command.env_remove("HOME");
            }
        }
        let result = output(&mut command);
        assert!(!result.status.success());
        assert!(stderr(&result).contains("HOME is not set or is empty"));
    }

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(home.join(".dof")).unwrap();
    let missing_bin = output(dof(&home).args(["run", "missing"]));
    assert!(!missing_bin.status.success());
    assert!(
        stderr(&missing_bin).contains(".dof/bin"),
        "{}",
        stderr(&missing_bin)
    );

    let fixture = Fixture::new();
    let missing_script = output(dof(&fixture.home).args(["run", "missing"]));
    assert!(!missing_script.status.success());
    assert!(
        stderr(&missing_script).contains("missing"),
        "{}",
        stderr(&missing_script)
    );
}

#[test]
fn rejects_non_executable_directories_and_symlinks() {
    let fixture = Fixture::new();

    let non_executable = fixture.bin.join("non-executable");
    fs::write(&non_executable, "#!/bin/sh\nexit 0\n").unwrap();
    set_mode(&non_executable, 0o644);
    let result = output(dof(&fixture.home).args(["run", "non-executable"]));
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("not executable"),
        "{}",
        stderr(&result)
    );

    fs::create_dir(fixture.bin.join("directory")).unwrap();
    let result = output(dof(&fixture.home).args(["run", "directory"]));
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("not a regular file"),
        "{}",
        stderr(&result)
    );

    let target = fixture.root.path().join("external-script");
    write_executable(&target, "#!/bin/sh\nexit 0\n");
    symlink(&target, fixture.bin.join("linked")).unwrap();
    let result = output(dof(&fixture.home).args(["run", "linked"]));
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("symlink") || stderr(&result).contains("not a regular file"),
        "{}",
        stderr(&result)
    );
}

#[test]
fn rejects_symlinked_state_and_bin_directories_without_executing_targets() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("unexpected-execution");
    let external_state = root.path().join("external-state");
    fs::create_dir_all(external_state.join("bin")).unwrap();
    write_executable(
        &external_state.join("bin/script"),
        &format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    );
    let state_home = root.path().join("state-home");
    fs::create_dir(&state_home).unwrap();
    symlink(&external_state, state_home.join(".dof")).unwrap();

    let result = output(dof(&state_home).args(["run", "script"]));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real directory"));
    assert!(!marker.exists());

    let bin_home = root.path().join("bin-home");
    fs::create_dir_all(bin_home.join(".dof")).unwrap();
    symlink(external_state.join("bin"), bin_home.join(".dof/bin")).unwrap();

    let result = output(dof(&bin_home).args(["run", "script"]));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real directory"));
    assert!(!marker.exists());
}

#[test]
fn rejects_path_traversal_absolute_paths_and_subpaths() {
    let fixture = Fixture::new();
    let marker = fixture.root.path().join("unexpected-execution");
    let escaped = fixture.home.join(".dof/escaped");
    write_executable(
        &escaped,
        &format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    );
    let nested = fixture.bin.join("nested");
    fs::create_dir(&nested).unwrap();
    write_executable(
        &nested.join("script"),
        &format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    );

    for script_name in [
        "../escaped",
        "nested/script",
        "./nested",
        ".",
        "..",
        "/bin/sh",
    ] {
        let result = output(dof(&fixture.home).args(["run", script_name]));
        assert!(
            !result.status.success(),
            "unsafe script name {script_name:?} unexpectedly succeeded"
        );
        assert!(
            stderr(&result).contains("script name")
                || stderr(&result).contains("single path component"),
            "unexpected error for {script_name:?}: {}",
            stderr(&result)
        );
        assert!(
            !marker.exists(),
            "unsafe script name {script_name:?} escaped the bin directory"
        );
    }
}

struct Fixture {
    root: TempDir,
    home: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin = home.join(".dof/bin");
        fs::create_dir_all(&bin).unwrap();
        Self { root, home, bin }
    }

    fn write_script(&self, name: &str, contents: &str) {
        write_executable(&self.bin.join(name), contents);
    }
}
