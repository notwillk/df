# Release procedure

`dof` releases are created from an annotated `vX.Y.Z` tag. The tag must match
the version in the root `[workspace.package]` manifest exactly.

## One-time signing setup

1. Generate a dedicated OpenPGP signing key that can be used
   non-interactively. Do not reuse a personal key.
2. Verify and record its full fingerprint through an independent channel.
3. Commit only its ASCII-armored public half as `keys/signing-key.asc`.
4. Add the matching ASCII-armored, unencrypted private key as the repository
   secret `GPG_PRIVATE_KEY`.
5. Authenticate GitHub CLI with an identity allowed to list this repository's
   Actions secrets; the local helper checks that `GPG_PRIVATE_KEY` exists.
6. Review the key-specific guidance in [`keys/README.md`](keys/README.md).

Neither the release helper nor the GitHub workflow accepts unsigned output.
The workflow also checks that the secret private key corresponds to the
committed public key before signing the manifest.

## Prepare and publish

Start on a clean `main` whose commit exactly matches `origin/main`. Confirm
that `gh auth status` succeeds and that your identity may list repository
Actions secrets, then run:

```sh
apps/cli/scripts/release.sh patch
```

Use `minor` or `major` instead of `patch` as appropriate. The helper:

1. Rejects dirty, detached, stale, or divergent branches and existing local
   or remote release tags.
2. Updates `[workspace.package].version` in `Cargo.toml` and refreshes the root
   `Cargo.lock`.
3. Runs formatting, strict Clippy, locked Rust tests, and a diff check that
   confirms the manifest and lockfile are the only pre-commit changes.
4. Restores the version files if validation fails before the release commit.
5. Commits as `Release vX.Y.Z`, creates an annotated tag, and atomically
   pushes `main` and the tag.

The tag-triggered workflow then tests on Linux and macOS, builds all three
artifacts, verifies them on native x86-64, ARM64 Linux, and ARM64 macOS
runners, signs the merged checksum manifest, and creates a non-draft GitHub
Release with generated notes. Any failed test, build, platform check,
checksum, or signature stops publication.

## Published artifacts

| Asset | Platform |
| --- | --- |
| `dof_linux_x86_64.tar.gz` | Linux x86-64, static musl |
| `dof_linux_aarch64.tar.gz` | Linux ARM64, static musl |
| `dof_darwin_aarch64.tar.gz` | Apple Silicon macOS |
| `checksums.txt` | SHA-256 manifest for all archives |
| `checksums.txt.sig` | Detached OpenPGP signature of the manifest |

Every archive contains exactly one executable named `dof`. Intel macOS,
Windows, crates.io publication, SBOMs, attestations, Apple code signing, and
notarization are outside the current release contract.

macOS support tracks GitHub Actions' `macos-latest` ARM runner. Its minimum
supported macOS version may therefore move when GitHub updates that image.

## Verify a release manually

Download an archive plus `checksums.txt`, `checksums.txt.sig`, and the public
key from `keys/signing-key.asc` at the same release tag. Use a temporary GPG
home so verification does not modify the user's normal keyring:

```sh
verification_home=$(mktemp -d)
chmod 700 "$verification_home"
gpg --homedir "$verification_home" --import signing-key.asc
gpg --homedir "$verification_home" --verify checksums.txt.sig checksums.txt
```

On Linux, verify the selected archive with:

```sh
sha256sum -c checksums.txt --ignore-missing
```

On macOS, compare the selected line in `checksums.txt` with:

```sh
shasum -a 256 dof_darwin_aarch64.tar.gz
```

Confirm the imported key's full fingerprint against the independently
recorded fingerprint before trusting the signature. Remove the temporary GPG
home after verification.

## Installer contract

The curl installer resolves `DOF_VERSION=latest` through GitHub's latest
release API; an explicit version must be an exact `vX.Y.Z` tag. `DEST` is a
directory and defaults to `/usr/local/bin`. The installer refuses unsupported
systems, a missing release-tagged public key, an invalid detached signature,
a missing or duplicate archive entry in the manifest, and any checksum
mismatch.

The installer contract can be exercised without network or privilege changes:

```sh
scripts/tests/install.sh
```
