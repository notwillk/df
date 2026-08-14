# dof workspace format

Consult this reference when creating, reviewing, or diagnosing workspace
features. The repository checkout lives at `$HOME/.dof/workspace`.

## Layout and feature selection

Feature declarations live only in `<workspace>/features/`. Each real directory
immediately inside that container is a feature. `default` is enabled when it
has no explicit setting, but is not required. Repository-level content outside
`features/` is not interpreted as a feature, so documentation and support
scripts may live alongside it. A
missing or empty `features/` directory represents a workspace with no
features. Pass the workspace repository root—not the `features/` directory—to
`dof lint`. A feature may contain any of these declarations:

```text
workspace/
├── README.md
└── features/
    ├── default/
    │   ├── home/
    │   │   ├── .bashrc
    │   │   └── .config/tool/settings.yaml
    │   ├── drop-ins/
    │   │   └── .Brewfile.d/
    │   │       └── 10-base
    │   ├── snippets.yaml
    │   └── apply
    └── work/
        ├── home/
        │   └── .gitconfig
        └── drop-ins/
            └── .Brewfile.d/
                └── 20-work
```

Feature selection is machine-local in `$HOME/.dof/config.yaml`:

```yaml
repo:
  url: https://github.com/example/dotfiles.git
  branch: main
  endpoint_fingerprint: "sha256:..."
features:
  default: true
  work: false
```

The `repo` mapping is maintained by `dof clone`; do not hand-edit its endpoint
fingerprint. An omitted `features` mapping or omitted `default` key enables the
`default` feature. Every other feature requires an explicit `true` setting.
Explicit values always take precedence, and lint still validates disabled
features.
Use `dof feature enable <name>` and `dof feature disable <name>` to change these
values.

## Complete-file ownership with `home/`

Every path beneath `features/<feature>/home/` maps to the same relative path
beneath the real `$HOME`:

```text
features/default/home/.config/tool/settings.yaml
    -> $HOME/.config/tool/settings.yaml
```

Missing `home/` directories are valid. Real regular files, hidden files,
nested directories, and empty directories are supported. Source symlinks and
special files are rejected. Existing destination directories are merged: dof
creates required entries but never clears unrelated files from a directory.

A complete file can be owned by only one feature. Features may share directory
paths when their files are distinct. Lint rejects duplicate file targets and
structural conflicts, such as one feature declaring `.config/tool` as a file
while another declares `.config/tool/settings.yaml`.

Never create a feature named `.dof` or a payload rooted at
`features/<feature>/home/.dof`. Dof state, configuration, workspaces,
binaries, and backups cannot manage themselves.

## Whole-file compilation with `drop-ins/`

Use `features/<feature>/drop-ins/` when multiple features should contribute
ordered fragments to one authoritative HOME file. A terminal directory ending
in `.d` is a compilation unit. Strip exactly that final suffix from its
HOME-relative path to find the output:

```text
features/default/drop-ins/.Brewfile.d/10-base
features/work/drop-ins/.Brewfile.d/20-work
    -> $HOME/.Brewfile

features/default/drop-ins/.config/systemd/user/example.service.d/override.conf.d/10-base
    -> $HOME/.config/systemd/user/example.service.d/override.conf
```

The `example.service.d` component remains because only the terminal
`override.conf.d` suffix is stripped. Directory components beneath `drop-ins/`
must be valid UTF-8. Targets must be nonempty safe relative paths, cannot begin
with any ASCII case variant of `.dof`, and cannot collide by ASCII case with
another managed target or implied parent when a drop-in is involved.

A fragment name must match this exact grammar:

```text
^[0-9]{2}-[a-z0-9][a-z0-9._-]*$
```

Each fragment must be a nonempty UTF-8 regular file, contain no NUL byte, and
end in a newline. Source symlinks and special files are invalid; fragment
permission bits do not affect the generated target. Within a terminal
directory, only fragment files are allowed. An intermediate directory may
contain only child directories. Mixed files and directories, orphan files,
and empty nested directories are errors. The top-level `drop-ins/` directory
itself may be missing or empty.

For one target, every fragment's two-digit numeric order and complete filename
are reserved globally across all features, including disabled features. Lint
therefore rejects two `10-*` fragments for the same target even if only one
feature is enabled. At apply time, dof selects enabled contributors, sorts them
solely by numeric order, and concatenates their exact bytes with no separators,
headers, or implicit existing-file prefix. The result owns the target's complete
contents.

Several features may contribute uniquely ordered fragments to one target. A
drop-in target cannot also be supplied by a `home/` payload or managed by
snippets, and it participates in the same file/ancestor structural collision
checks as other whole-file resources.

A missing output is created with mode `0600`. An existing regular output keeps
its mode and is left untouched when its bytes already match. A changed regular
file is backed up and atomically replaced. A destination leaf symlink is backed
up as a link, then replaced without following it by a regular `0600` file.

Changing or disabling one contributor recompiles the target from the remaining
enabled fragments on the next apply. If the last contributor is disabled or
removed, dof leaves the last generated file in place because it does not keep
persistent ownership state.

## Append-if-absent text with `snippets.yaml`

An optional `features/<feature>/snippets.yaml` is a real UTF-8 regular file
containing a top-level `snippets` mapping. Each key is a safe `$HOME`-relative
target and each value is an array of strings:

```yaml
snippets:
  .bashrc:
    - export EDITOR=vim
    - |
      if command -v starship >/dev/null 2>&1; then
        eval "$(starship init bash)"
      fi
  .config/git/config:
    - |
      [pull]
          rebase = true
```

Each string, including a multiline block, is tested as an exact substring of
the current target. Missing strings are appended in lexical feature order and
YAML array order, with newline boundaries added when needed. Reapplying the
same declarations does not append an already present string.

Multiple features may contribute snippets to the same target. A
snippet-managed file cannot also be supplied by any feature's `home/` tree,
compiled from drop-ins, or participate in a file/ancestor structural
collision. Snippet targets must be nonempty relative paths containing no
traversal and cannot begin with `.dof`. Arrays must contain YAML strings;
numeric and other scalar types are schema errors.

An existing snippet target must be a regular UTF-8 file; a destination symlink
is rejected rather than followed. Dof preserves an existing target's mode and
creates a missing snippet target with mode `0600`.

## Imperative `apply` hook

An optional `features/<feature>/apply` must be a real regular file, start with
`#!`, and have an execute bit. For example:

```sh
#!/bin/sh
set -eu

if ! command -v example-tool >/dev/null 2>&1; then
  curl --fail --location https://example.invalid/install.sh | sh
fi
```

Make every action idempotent. Enabled hooks run on every `dof apply`, after all
copied files, compiled drop-in targets, and snippets, in lexical feature-name
order. A hook runs with its feature directory as the working directory and
inherits the user's process environment and standard streams. A nonzero result
stops later hooks. A hook may modify a generated target, but a later apply
reconciles that file to its compiled content while contributors remain active.

Do not add network installers like the example without the user's informed
authorization and an appropriate trust review.

## Reconciliation and recovery

Before mutation, apply validates declarations from all features, including
disabled ones, and preflights destination shapes. Destination ancestor
symlinks and file/directory conflicts fail before home-directory changes.

Unchanged copied or compiled files are skipped. New files do not need backups.
Before replacing changed existing files, dof mirrors the prior leaf into one
private timestamped snapshot under `$HOME/.dof/backups/`; copied-file and
drop-in destination symlinks are backed up as links and replaced without
following them. All declarative changes in one invocation share that lazy
snapshot. Writes are staged beside their destination and installed atomically
per file.

The entire apply is not a transaction. If later I/O or an imperative hook
fails, earlier successful changes and their backups remain.
