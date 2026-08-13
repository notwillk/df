#!/usr/bin/env bash

set -Eeuo pipefail

readonly PROGRAM_NAME="$(basename "$0")"
readonly RELEASE_BRANCH="main"
readonly RELEASE_REMOTE="${DOF_RELEASE_REMOTE:-origin}"
readonly RELEASE_REPOSITORY="${DOF_RELEASE_REPOSITORY:-notwillk/dof}"
readonly CARGO_BIN="${DOF_RELEASE_CARGO:-cargo}"
readonly GPG_BIN="${DOF_RELEASE_GPG:-gpg}"
readonly GH_BIN="${DOF_RELEASE_GH:-gh}"

usage() {
  cat >&2 <<EOF
Usage: $PROGRAM_NAME patch|minor|major

Create and atomically publish a dof release from an up-to-date, clean main branch.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

read_workspace_version() {
  awk '
    /^[[:space:]]*\[workspace\.package\][[:space:]]*(#.*)?$/ {
      in_workspace_package = 1
      next
    }
    /^[[:space:]]*\[/ {
      in_workspace_package = 0
    }
    in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
      if ($0 !~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*(#.*)?$/) {
        exit 2
      }

      value = $0
      sub(/^[[:space:]]*version[[:space:]]*=[[:space:]]*"/, "", value)
      sub(/"[[:space:]]*(#.*)?$/, "", value)
      print value
      found += 1
    }
    END {
      if (found != 1) {
        exit 3
      }
    }
  ' Cargo.toml
}

write_workspace_version() {
  local version="$1"
  local temporary

  temporary="$(mktemp "$workspace_root/.Cargo.toml.release.XXXXXX")"
  cp -p Cargo.toml "$temporary"

  if ! awk -v version="$version" '
    /^[[:space:]]*\[workspace\.package\][[:space:]]*(#.*)?$/ {
      in_workspace_package = 1
    }
    /^[[:space:]]*\[/ && $0 !~ /^[[:space:]]*\[workspace\.package\][[:space:]]*(#.*)?$/ {
      in_workspace_package = 0
    }
    in_workspace_package && /^[[:space:]]*version[[:space:]]*=/ {
      if ($0 !~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*(#.*)?$/) {
        exit 2
      }
      if (found != 0) {
        exit 3
      }

      sub(/"[^"]+"/, "\"" version "\"")
      found = 1
    }
    { print }
    END {
      if (found != 1) {
        exit 4
      }
    }
  ' Cargo.toml >"$temporary"; then
    rm -f -- "$temporary"
    die 'could not update [workspace.package].version in Cargo.toml'
  fi

  mv -- "$temporary" Cargo.toml
}

bump_version() {
  local current="$1"
  local bump="$2"
  local major minor patch extra

  IFS=. read -r major minor patch extra <<<"$current"
  if [[ -n "${extra:-}" ]] ||
    [[ ! "$major" =~ ^(0|[1-9][0-9]*)$ ]] ||
    [[ ! "$minor" =~ ^(0|[1-9][0-9]*)$ ]] ||
    [[ ! "$patch" =~ ^(0|[1-9][0-9]*)$ ]]; then
    die "workspace version must be an exact X.Y.Z semantic version, found: $current"
  fi

  case "$bump" in
    patch)
      patch=$((patch + 1))
      ;;
    minor)
      minor=$((minor + 1))
      patch=0
      ;;
    major)
      major=$((major + 1))
      minor=0
      patch=0
      ;;
    *)
      usage
      exit 2
      ;;
  esac

  printf '%s.%s.%s\n' "$major" "$minor" "$patch"
}

if [[ $# -ne 1 ]]; then
  usage
  exit 2
fi

case "$1" in
  patch | minor | major) ;;
  *)
    usage
    exit 2
    ;;
esac

require_command git
require_command "$CARGO_BIN"
require_command "$GPG_BIN"
require_command "$GH_BIN"

[[ "$RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
  die "release repository must be an OWNER/REPO name, found: $RELEASE_REPOSITORY"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
workspace_root="$(cd -- "$script_dir/../../.." && pwd -P)"
cd -- "$workspace_root"

git_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die 'release helper must run from a Git worktree'
git_root="$(cd -- "$git_root" && pwd -P)"
[[ "$git_root" == "$workspace_root" ]] || die 'release helper is not located in the repository root it would release'

git ls-files --error-unmatch -- Cargo.toml Cargo.lock >/dev/null 2>&1 ||
  die 'Cargo.toml and Cargo.lock must both be committed before releasing'

readonly SIGNING_KEY="keys/signing-key.asc"
[[ -s "$SIGNING_KEY" ]] || die "$SIGNING_KEY is required before releasing"
git ls-files --error-unmatch -- "$SIGNING_KEY" >/dev/null 2>&1 ||
  die "$SIGNING_KEY must be committed before releasing"
grep -q '^-----BEGIN PGP PUBLIC KEY BLOCK-----$' "$SIGNING_KEY" ||
  die "$SIGNING_KEY is not an armored PGP public key"

current_branch="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
  die 'releases cannot be created from a detached HEAD'
[[ "$current_branch" == "$RELEASE_BRANCH" ]] ||
  die "releases must be created from $RELEASE_BRANCH (currently $current_branch)"

[[ -z "$(git status --porcelain=v1 --untracked-files=normal)" ]] ||
  die 'the worktree must be clean before releasing'

git remote get-url "$RELEASE_REMOTE" >/dev/null 2>&1 ||
  die "Git remote does not exist: $RELEASE_REMOTE"

printf 'Fetching %s...\n' "$RELEASE_REMOTE"
git fetch --prune --no-tags "$RELEASE_REMOTE"

remote_branch_ref="refs/remotes/$RELEASE_REMOTE/$RELEASE_BRANCH"
git rev-parse --verify --quiet "$remote_branch_ref^{commit}" >/dev/null ||
  die "$RELEASE_REMOTE/$RELEASE_BRANCH does not exist"

local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse "$remote_branch_ref")"
[[ "$local_head" == "$remote_head" ]] ||
  die "$RELEASE_BRANCH must exactly match $RELEASE_REMOTE/$RELEASE_BRANCH before releasing"

if ! current_version="$(read_workspace_version)"; then
  die 'Cargo.toml must contain exactly one quoted version in [workspace.package]'
fi
next_version="$(bump_version "$current_version" "$1")"
tag="v$next_version"

git show-ref --verify --quiet "refs/tags/$tag" && die "local tag already exists: $tag"
remote_tag="$(git ls-remote --tags "$RELEASE_REMOTE" "refs/tags/$tag" "refs/tags/$tag^{}")" ||
  die "could not inspect tags on $RELEASE_REMOTE"
[[ -z "$remote_tag" ]] || die "remote tag already exists: $tag"

temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/dof-release.XXXXXX")"
rollback_pending=0

cleanup() {
  local status=$?
  trap - EXIT

  if [[ $status -ne 0 && $rollback_pending -eq 1 ]]; then
    cp -p "$temporary_root/Cargo.toml" Cargo.toml
    cp -p "$temporary_root/Cargo.lock" Cargo.lock
    printf 'Release preparation failed; restored Cargo.toml and Cargo.lock.\n' >&2
  fi

  rm -rf -- "$temporary_root"
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' HUP INT TERM

mkdir -m 700 "$temporary_root/gnupg"
if ! GNUPGHOME="$temporary_root/gnupg" "$GPG_BIN" --batch --quiet --show-keys "$SIGNING_KEY" >/dev/null 2>&1; then
  die "$SIGNING_KEY cannot be parsed by GPG"
fi

if ! private_key_secret="$("$GH_BIN" secret list \
  --app actions \
  --repo "$RELEASE_REPOSITORY" \
  --json name \
  --jq 'any(.[]; .name == "GPG_PRIVATE_KEY")')"; then
  die "could not inspect Actions secrets for $RELEASE_REPOSITORY with GitHub CLI"
fi
[[ "$private_key_secret" == true ]] ||
  die "GPG_PRIVATE_KEY is not configured as an Actions secret for $RELEASE_REPOSITORY"

cp -p Cargo.toml "$temporary_root/Cargo.toml"
cp -p Cargo.lock "$temporary_root/Cargo.lock"
rollback_pending=1

printf 'Preparing dof %s...\n' "$next_version"
write_workspace_version "$next_version"
"$CARGO_BIN" update --package dof --precise "$next_version"

git diff --quiet -- Cargo.toml && die 'Cargo.toml did not change during the version bump'
git diff --quiet -- Cargo.lock && die 'Cargo.lock did not change during the version bump'

printf 'Running release validation...\n'
"$CARGO_BIN" fmt --all --check
"$CARGO_BIN" clippy --workspace --all-targets --all-features --locked -- -D warnings
"$CARGO_BIN" test --workspace --all-features --locked --no-fail-fast
git diff --check

git commit -m "Release $tag" -- Cargo.toml Cargo.lock
rollback_pending=0

git tag -a "$tag" -m "Release $tag"

printf 'Atomically pushing %s and %s...\n' "$RELEASE_BRANCH" "$tag"
git push --atomic "$RELEASE_REMOTE" "$RELEASE_BRANCH" "refs/tags/$tag"

printf 'Released %s.\n' "$tag"
