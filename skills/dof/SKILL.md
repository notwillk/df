---
name: dof
description: Operate dof and author dof dotfiles workspaces. Use when users mention dof, ~/.dof, dof clone/apply/lint/features/feature/run, feature home payloads, snippets.yaml, feature apply hooks, or need help diagnosing dof validation, ownership, collision, backup, or execution errors. Do not use for the POSIX df disk-space command or unrelated dotfile managers.
---

# dof

Use `dof` to inspect, validate, and reconcile a trusted Git-backed dotfiles
workspace with the user's home directory.

## Start safely

1. Check that `dof` is available with `command -v dof` and `dof --version`.
   If it is absent, explain that it must be installed and offer the
   [official installation instructions](https://github.com/notwillk/dof#install).
   Do not install it silently.
2. Consult `dof --help` and the relevant `dof <command> --help` before acting.
   Prefer current CLI help over remembered flags.
3. Resolve managed paths from the user's actual `$HOME`. The state directory is
   `$HOME/.dof`, with the checkout at `$HOME/.dof/workspace` and configuration
   at `$HOME/.dof/config.yaml`.
4. Treat inspection and linting as read-only. Run `clone`, `apply`, feature
   toggles, or managed scripts only when the user's request authorizes the
   corresponding mutation or execution.
5. Treat the workspace as trusted code. Feature `apply` hooks and programs
   invoked by `dof run` execute with the user's environment and privileges.

Treat a normal `dof clone` as a mutation when managed state already exists: a
matching checkout may be fast-forwarded. Run it only when the user authorizes
initializing or updating that repository. Never use `dof clone --force` unless
the user explicitly intends to destroy and replace the existing managed
workspace and config.

## Inspect and validate

Use JSON when an agent needs to consume the enabled-feature list:

```sh
dof features --json
```

Validate the workspace before applying it and after making workspace edits:

```sh
dof lint "$HOME/.dof/workspace"
```

Linting checks every feature, including disabled features. Do not assume a
disabled feature can contain an invalid schema or ownership conflict. Before
an apply, also review the enabled features' imperative hooks; lint validates
their shape and permissions, not whether their behavior is safe.

## Initialize or update a workspace

Clone or update a repository only when the user asks to initialize or update
their dof state:

```sh
dof clone [--branch <branch>] <repository>
```

The branch defaults to `main`. With no workspace or config, this creates a
fresh installation. With both present, the configured repository and branch
must match the command. `dof` then validates the checkout, fetches with the
system Git executable, and fast-forwards only when the fetched branch is ahead.
An already-current or locally-ahead checkout is a successful no-op and is never
rewound. Stable Git `url.*.insteadOf` aliases are supported; dof binds the
effective endpoint during a fresh clone and rejects later changes to that
resolution.

Expect a normal update to fail on divergent history, destructive checkout
conflicts, a different repository or branch, partial state, malformed state,
or a configured URL that now resolves to a different endpoint. Diagnose those
conditions instead of escalating automatically to
`--force`. If the intended repository or branch is unclear, inspect
`$HOME/.dof/config.yaml` read-only and confirm the desired source before
running the command. Before considering `--force`, explain that it deletes the
existing workspace and config without restoring them if the new clone fails.

## Author workspace features

Read [the workspace format reference](references/workspace-format.md) before
creating or changing feature declarations. Choose one mechanism for each need:

- Put each feature at `features/<feature>/`; repository content outside
  `features/` does not declare features. The container may be absent or empty
  when the workspace has no features.
- Put a complete file under `features/<feature>/home/**` when dof should own
  its full contents.
- Put exact append-if-absent text in `features/<feature>/snippets.yaml` when
  dof should preserve unrelated content in a text file.
- Put imperative setup in an executable, shebang-bearing
  `features/<feature>/apply` hook only when declarative files or snippets are
  insufficient. Make the hook idempotent because it runs on every apply.

Never declare anything under `$HOME/.dof` through a `home/` payload or snippet.
After editing, run lint and fix all errors before proposing an apply.

## Enable and disable features

Use the CLI rather than editing feature booleans by hand:

```sh
dof feature enable <feature>
dof feature disable <feature>
```

The feature must already be a real directory immediately beneath the
workspace's `features/` directory. An omitted `default` setting is enabled;
every other omitted feature is disabled. Explicit settings always take
precedence.

## Apply safely

When the user authorizes reconciliation, lint first and then apply:

```sh
dof lint "$HOME/.dof/workspace"
dof apply
```

`dof apply` validates the whole workspace, projects the enabled features, then
processes copied files, snippets, and hooks in deterministic order. It merges
directories without deleting unrelated files. It skips unchanged managed
files and places backups for changed existing files in one timestamped
`$HOME/.dof/backups/` snapshot.

File replacement is atomic per file, not transactional for the whole command.
If a later operation or hook fails, earlier changes and their backups remain.
Report the failure, the completed work, and any printed backup path; do not
claim that the home directory was rolled back.

## Run a managed program

When the user asks to execute a trusted program from `$HOME/.dof/bin`, use:

```sh
dof run <script> [arguments]...
```

The script name is a basename, not a path. `dof` forwards arguments, standard
streams, environment, current directory, exit status, and signals. Do not use
this command merely to inspect a script; read it first when inspection is what
the user requested.

## Troubleshoot

- Start with the exact failing command's help and `dof lint` diagnostics.
- For ownership errors, identify every feature claiming the reported target
  and choose either copy ownership or snippet management as allowed by the
  workspace format.
- For an apply failure, inspect the reported backup snapshot and determine
  which earlier files changed before retrying. A failing hook may have made
  arbitrary partial changes.
- Distinguish `dof` from POSIX `df`: `df` reports filesystem space and is not
  covered by this skill.
