# `dof`

`dof` maintains dotfiles from a Git repository.

## Install

Release binaries are available for Linux x86-64, Linux ARM64, and Apple
Silicon macOS. The installer requires `curl`, `tar`, and a platform SHA-256
utility. By default, it also requires either GnuPG (`gpg`) or a compatible
Sequoia `sq` for signature verification:

```sh
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://raw.githubusercontent.com/notwillk/dof/main/scripts/install.sh | sh
```

It installs the latest release to `/usr/local/bin`. Set `DOF_VERSION` to an
exact release tag and `DEST` to a destination directory when needed:

```sh
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://raw.githubusercontent.com/notwillk/dof/main/scripts/install.sh | \
  DOF_VERSION=v0.1.0 DEST="$HOME/.local/bin" sh
```

The installer downloads the archive, `checksums.txt`, its detached OpenPGP
signature, and the public key committed at the selected release tag. It
prefers `gpg` and otherwise uses a compatible Sequoia `sq` to verify the
manifest signature, then verifies the selected archive's exact SHA-256 entry
before installing. On macOS, either verifier can be installed with Homebrew:

```sh
brew install gnupg
# or
brew install sequoia-sq
```

By default, the installer fails closed if no compatible verifier is available,
the signing assets cannot be downloaded, or signature verification fails. To
continue after such a failure, pass `--ignore-signature-validation` to the
shell running the installer:

```sh
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://raw.githubusercontent.com/notwillk/dof/main/scripts/install.sh | \
  sh -s -- --ignore-signature-validation
```

The flag does not skip an available verifier: verification is still attempted,
and the installer warns only if it must ignore a failure. SHA-256 and archive
safety checks always remain mandatory. **When signature validation is ignored,
the checksum manifest is unauthenticated and therefore cannot protect against
an attacker who can replace both the archive and its checksum.**

An outer installer, including `notwillk/dotfiles`'s `install.sh`, must
explicitly forward `--ignore-signature-validation` to the `dof` installer;
passing it only to a wrapper has no effect unless that wrapper forwards it.
`sudo` is used only when the destination directory cannot be written directly.

Intel macOS, Windows, and other platforms are not supported. Release signing
is not operational until the repository maintainer completes the setup in
[`keys/README.md`](keys/README.md); until then, the strict installer and
release workflow intentionally fail rather than accepting unsigned binaries.

## Agent skill

Install the repository's agent-neutral `dof` skill at user scope. Set
`AGENT_SKILL_HOST` to the host identifier accepted by `gh skill` for your
agentic coding tool:

```sh
gh skill install notwillk/dof dof \
  --agent "$AGENT_SKILL_HOST" \
  --scope user
```

Use `gh skill install --help` to see the host identifiers supported by the
installed GitHub CLI.

## Development

The CLI is a Rust workspace application under `apps/cli`.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
```

## Clone or update a dotfiles repository

```sh
dof clone [-b|--branch <branch>] [--force] <repository>
```

The branch defaults to `main`. The repository is cloned to
`$HOME/.dof/workspace`, and its source is recorded in `$HOME/.dof/config.yaml`:

```yaml
repo:
  url: https://github.com/example/dotfiles.git
  branch: main
  endpoint_fingerprint: "sha256:..."
features: {}
```

When both managed paths are absent, `dof clone` creates a fresh checkout and
config. When both already exist and their configured repository and branch
match the command, `dof clone` validates the checkout, fetches that branch, and
fast-forwards it when the remote is ahead. A checkout that is already current
or has local commits ahead of the fetched branch is left unchanged; `dof`
never rewinds it. Updating an existing checkout requires the system `git`
executable.

The configured URL is matched literally. On a fresh clone, dof also records a
SHA-256 fingerprint of the effective endpoint after Git URL aliases are
applied. HTTP(S) user information and URL passwords are removed before
hashing. Credentials placed elsewhere in a URL, such as its path or query
text, are not guaranteed to be excluded and should not be embedded in
repository URLs. A stable `url.*.insteadOf` alias continues to work, while
adding, removing, or changing an applicable alias fails before fetching from a
different endpoint.

The update fails rather than performing a destructive checkout when the local
and fetched histories have diverged or local files prevent the fast-forward.
It also fails when the requested repository or branch differs from the config,
when only one managed path exists, or when the existing state is malformed.

Pass `--force` to delete the existing workspace and config regardless of their
contents and create a fresh installation. This is destructive: the previous
state is not backed up or restored if the new clone fails.

## List enabled features

Feature definitions live under `$HOME/.dof/workspace/features`. Each real
directory immediately inside that directory is a feature. The `default`
feature is enabled when it has no explicit setting, but no feature is
required. Repository content outside `features/`, plus files and symlinks
inside it, is not treated as a feature. An absent or empty `features/`
directory represents a workspace with no features.

Feature overrides live in `$HOME/.dof/config.yaml`:

```yaml
features:
  default: true
  work: false
```

An omitted `features` section or omitted `default` setting enables the
`default` feature. Every other feature is disabled unless its setting is
explicitly `true`. Explicit settings always take precedence, so `default` can
also be disabled.

```sh
dof features
dof features --json
```

Both formats list enabled features in lexical order. Text output contains one
name per line; `--json` emits an array of strings.

## Enable or disable a feature

```sh
dof feature enable <feature>
dof feature disable <feature>
```

The named feature must already exist as a real directory at
`$HOME/.dof/workspace/features/<feature>`. These commands write an explicit
`true` or `false` entry for the feature in `$HOME/.dof/config.yaml`.

Feature settings determine which features are selected by `dof features` and
`dof apply`. Workspace linting continues to validate every feature, including
disabled features.

## Apply enabled features

Files under each enabled feature's `home/` directory map directly into
`$HOME`:

```text
$HOME/.dof/workspace/
└── features/
    ├── default/
    │   └── home/
    │       ├── .bashrc
    │       └── .config/tool/settings.yaml
    └── work/
        └── home/
            └── .gitconfig
```

```sh
dof apply
```

`dof apply` validates the complete workspace before changing the home
directory, then copies files from enabled features. It creates missing and
empty directories, preserves regular-file permission bits, and skips files
whose contents and permissions already match.

Changed files and destination symlinks are backed up without following links
under one private snapshot:

```text
$HOME/.dof/backups/<UTC timestamp>/<home-relative path>
```

Each file is staged in its destination directory and installed atomically.
Unexpected failures can leave earlier files applied; their backups remain
available. Source symlinks and special files are not supported.

Each feature may also contain a `snippets.yaml` file. Its `snippets` mapping
uses `$HOME`-relative target paths as keys and arrays of required text as
values:

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

For enabled features, `dof apply` checks every listed string and appends it
only when the complete string is absent from the target file. This also makes
multiline snippets idempotent across repeated applies. Multiple features may
contribute snippets to the same target file. Existing target permissions are
preserved; newly created snippet targets use private `0600` permissions.

A target managed by snippets cannot also be supplied by any feature's
`home/` tree. Likewise, a `home/` file can be owned by at most one feature.
This keeps copy ownership deterministic while still allowing several features
to contribute independent snippets to one file.

Each feature may also provide an `apply` script at its root, for example
`features/default/apply`. It must be a regular executable file whose first
line is a shebang. Workspace linting validates these requirements for every
feature, including disabled features.

After all `home/` files have been copied and snippets have been applied, `dof
apply` runs the scripts from enabled features in lexical feature-name order.
Each script runs with its feature directory as the working directory and
inherits the `dof` process environment. Scripts run on every invocation, so
they are responsible for being idempotent. A nonzero exit status stops
execution immediately and makes `dof apply` fail; scripts for later features
are not run.

## Run a managed script

Executable scripts stored directly in `$HOME/.dof/bin` can be invoked by
basename through `dof`:

```sh
dof run xyz --verbose input.txt
```

This runs `$HOME/.dof/bin/xyz` with `--verbose` and `input.txt` as its
arguments. The script name must be a single basename, not a path, and the
target must be an executable regular file.

All arguments following the script name are passed through unchanged. The
script inherits standard input, standard output, standard error, the
environment, and the caller's current working directory. On Linux and macOS,
`dof` replaces itself with the script process, so the script receives signals
directly and its exit status is the exit status of `dof run`.

## Lint a dotfiles workspace

```sh
dof lint <directory>
```

Pass the workspace repository root to `dof lint`, not its `features/`
directory.

`dof lint` performs read-only validation across every feature, including
features disabled on the current machine. It rejects a feature named `.dof`,
payloads targeting `$HOME/.dof`, source symlinks and special files, duplicate
file destinations, copy/snippet ownership conflicts, and file/directory
collisions. It also validates each `snippets.yaml` schema and its
`$HOME`-relative targets. Different features may contribute distinct files
beneath the same directory or snippets to the same target.

## Releases

Pushing a tag that exactly matches the root workspace version publishes these
assets after native tests and artifact verification succeed:

- `dof_linux_x86_64.tar.gz`
- `dof_linux_aarch64.tar.gz`
- `dof_darwin_aarch64.tar.gz`
- `checksums.txt`
- `checksums.txt.sig`

Each archive contains one executable named `dof`. Linux binaries are static
musl executables; the macOS binary is ARM64. The checksum manifest is signed
with the dedicated release key. macOS support tracks GitHub Actions'
`macos-latest` ARM runner, so its minimum supported OS version may move as the
runner image changes.

Maintainers can prepare a patch, minor, or major release with:

```sh
apps/cli/scripts/release.sh patch
```

The helper requires a clean, synchronized `main` and an authenticated GitHub
CLI identity allowed to list repository Actions secrets. It verifies both the
committed public key and the `GPG_PRIVATE_KEY` secret before changing the
version, runs the local gates, creates an annotated tag, and atomically pushes
`main` with that tag. See
[`release-procedure.md`](release-procedure.md) for signing setup, manual
verification, and the complete release procedure.
