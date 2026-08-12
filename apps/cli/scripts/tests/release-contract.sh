#!/usr/bin/env bash

set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
release_script="$(cd -- "$script_dir/.." && pwd -P)/release.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dof-release-contract.XXXXXX")"

cleanup() {
  local status=$?
  trap - EXIT
  rm -rf -- "$test_root"
  exit "$status"
}
trap cleanup EXIT

fail() {
  printf 'not ok - %s\n' "$*" >&2
  exit 1
}

pass() {
  printf 'ok - %s\n' "$1"
}

assert_eq() {
  local expected="$1"
  local actual="$2"
  local message="$3"
  [[ "$actual" == "$expected" ]] ||
    fail "$message (expected '$expected', found '$actual')"
}

assert_clean() {
  local repository="$1"
  [[ -z "$(git -C "$repository" status --porcelain=v1 --untracked-files=normal)" ]] ||
    fail "expected a clean repository at $repository"
}

fake_gpg="$test_root/fake-gpg"
cat >"$fake_gpg" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$fake_gpg"

fake_gh="$test_root/fake-gh"
cat >"$fake_gh" <<'EOF'
#!/bin/sh
set -eu

[ "$#" -eq 10 ]
[ "$1" = secret ]
[ "$2" = list ]
[ "$3" = --app ]
[ "$4" = actions ]
[ "$5" = --repo ]
[ "$6" = fixture/dof ]
[ "$7" = --json ]
[ "$8" = name ]
[ "$9" = --jq ]
[ "${10}" = 'any(.[]; .name == "GPG_PRIVATE_KEY")' ]

case "${FAKE_GH_RESULT:-true}" in
  true | false)
    printf '%s\n' "$FAKE_GH_RESULT"
    ;;
  error)
    exit 1
    ;;
  *)
    exit 2
    ;;
esac
EOF
chmod +x "$fake_gh"

create_fixture() {
  local version="${1:-0.1.0}"
  local failing_test="${2:-false}"
  local fixture_root
  local repository
  local remote

  fixture_root="$(mktemp -d "$test_root/fixture.XXXXXX")"
  repository="$fixture_root/work"
  remote="$fixture_root/origin.git"
  mkdir -p "$repository/apps/cli/src" "$repository/apps/cli/scripts" "$repository/keys"
  git init -q --bare "$remote"
  git --git-dir="$remote" symbolic-ref HEAD refs/heads/main
  git init -q -b main "$repository"
  git -C "$repository" config user.name 'Release Contract'
  git -C "$repository" config user.email 'release-contract@example.invalid'

  cat >"$repository/Cargo.toml" <<EOF
[workspace]
members = ["apps/cli"]
resolver = "2"

[workspace.package]
version = "$version"
edition = "2021"
EOF

  cat >"$repository/apps/cli/Cargo.toml" <<'EOF'
[package]
name = "dof"
version.workspace = true
edition.workspace = true
EOF

  if [[ "$failing_test" == true ]]; then
    cat >"$repository/apps/cli/src/main.rs" <<'EOF'
fn main() {}

#[cfg(test)]
mod tests {
    #[test]
    fn release_gate_fails() {
        panic!("intentional release-contract failure");
    }
}
EOF
  else
    cat >"$repository/apps/cli/src/main.rs" <<'EOF'
fn main() {}

#[cfg(test)]
mod tests {
    #[test]
    fn release_gate_passes() {
        assert_eq!(2 + 2, 4);
    }
}
EOF
  fi

  cat >"$repository/.gitignore" <<'EOF'
/target/
EOF
  cat >"$repository/keys/signing-key.asc" <<'EOF'
-----BEGIN PGP PUBLIC KEY BLOCK-----
release contract fixture
-----END PGP PUBLIC KEY BLOCK-----
EOF
  cp "$release_script" "$repository/apps/cli/scripts/release.sh"
  chmod +x "$repository/apps/cli/scripts/release.sh"

  cargo generate-lockfile --manifest-path "$repository/Cargo.toml" >/dev/null 2>&1
  git -C "$repository" add .
  git -C "$repository" commit -q -m 'Initial fixture'
  git -C "$repository" remote add origin "$remote"
  git -C "$repository" push -q -u origin main

  printf '%s\n' "$repository"
}

run_release() {
  local repository="$1"
  local bump="$2"
  local secret_result="${3:-true}"
  (
    cd -- "$repository"
    DOF_RELEASE_GPG="$fake_gpg" \
      DOF_RELEASE_GH="$fake_gh" \
      DOF_RELEASE_REPOSITORY=fixture/dof \
      FAKE_GH_RESULT="$secret_result" \
      apps/cli/scripts/release.sh "$bump"
  )
}

read_manifest_version() {
  local repository="$1"
  awk '
    /^\[workspace\.package\]$/ { section = 1; next }
    /^\[/ { section = 0 }
    section && /^version = / {
      value = $0
      sub(/^version = "/, "", value)
      sub(/"$/, "", value)
      print value
    }
  ' "$repository/Cargo.toml"
}

read_lock_version() {
  local repository="$1"
  awk '
    /^name = "dof"$/ { dof = 1; next }
    dof && /^version = / {
      value = $0
      sub(/^version = "/, "", value)
      sub(/"$/, "", value)
      print value
      exit
    }
    /^\[\[package\]\]$/ { dof = 0 }
  ' "$repository/Cargo.lock"
}

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

for bump_case in 'patch:1.2.3:1.2.4' 'minor:1.2.3:1.3.0' 'major:1.2.3:2.0.0'; do
  IFS=: read -r bump initial expected <<<"$bump_case"
  repository="$(create_fixture "$initial")"
  run_release "$repository" "$bump" >/dev/null

  assert_eq "$expected" "$(read_manifest_version "$repository")" "$bump manifest bump"
  assert_eq "$expected" "$(read_lock_version "$repository")" "$bump lockfile bump"
  assert_eq "Release v$expected" "$(git -C "$repository" log -1 --format=%s)" "$bump commit message"
  assert_eq tag "$(git -C "$repository" cat-file -t "v$expected")" "$bump annotated tag"
  assert_eq "$(git -C "$repository" rev-parse HEAD)" \
    "$(git --git-dir="$(git -C "$repository" remote get-url origin)" rev-parse refs/heads/main)" \
    "$bump atomic branch push"
  git --git-dir="$(git -C "$repository" remote get-url origin)" \
    rev-parse --verify --quiet "refs/tags/v$expected^{tag}" >/dev/null ||
    fail "$bump tag was not pushed"
  assert_clean "$repository"
  pass "$bump release bumps, validates, tags, and atomically pushes"
done

repository="$(create_fixture)"
before="$(git -C "$repository" rev-parse HEAD)"
if run_release "$repository" banana >/dev/null 2>&1; then
  fail 'invalid bump was accepted'
fi
assert_eq "$before" "$(git -C "$repository" rev-parse HEAD)" 'invalid bump changed HEAD'
assert_clean "$repository"
pass 'invalid bump is rejected without changes'

repository="$(create_fixture)"
touch "$repository/untracked"
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'dirty worktree was accepted'
fi
[[ -f "$repository/untracked" ]] || fail 'dirty worktree check removed the user file'
pass 'dirty worktree is rejected without deleting changes'

repository="$(create_fixture)"
other="$test_root/stale-writer"
git clone -q "$(git -C "$repository" remote get-url origin)" "$other"
git -C "$other" config user.name 'Release Contract'
git -C "$other" config user.email 'release-contract@example.invalid'
printf 'remote advanced\n' >"$other/remote-only"
git -C "$other" add remote-only
git -C "$other" commit -q -m 'Advance remote main'
git -C "$other" push -q origin main
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'stale main was accepted'
fi
assert_eq 0.1.0 "$(read_manifest_version "$repository")" 'stale main changed manifest'
assert_clean "$repository"
pass 'main must exactly match fetched origin/main'

repository="$(create_fixture)"
git -C "$repository" tag -a v0.1.1 -m 'Existing local tag'
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'existing local tag was accepted'
fi
assert_eq 0.1.0 "$(read_manifest_version "$repository")" 'local tag rejection changed manifest'
pass 'existing local tag is rejected'

repository="$(create_fixture)"
git -C "$repository" tag -a v0.1.1 -m 'Existing remote tag'
git -C "$repository" push -q origin refs/tags/v0.1.1
git -C "$repository" tag -d v0.1.1 >/dev/null
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'existing remote tag was accepted'
fi
assert_eq 0.1.0 "$(read_manifest_version "$repository")" 'remote tag rejection changed manifest'
assert_clean "$repository"
pass 'existing remote tag is rejected'

repository="$(create_fixture 0.1.0 true)"
manifest_before="$(checksum "$repository/Cargo.toml")"
lock_before="$(checksum "$repository/Cargo.lock")"
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'failing validation gate was accepted'
fi
assert_eq "$manifest_before" "$(checksum "$repository/Cargo.toml")" \
  'validation failure did not restore Cargo.toml'
assert_eq "$lock_before" "$(checksum "$repository/Cargo.lock")" \
  'validation failure did not restore Cargo.lock'
assert_clean "$repository"
pass 'failed pre-commit validation restores both version files'

repository="$(create_fixture)"
git -C "$repository" rm -q keys/signing-key.asc
git -C "$repository" commit -q -m 'Remove signing key'
git -C "$repository" push -q origin main
if run_release "$repository" patch >/dev/null 2>&1; then
  fail 'missing signing key was accepted'
fi
pass 'committed signing key is required'

repository="$(create_fixture)"
manifest_before="$(checksum "$repository/Cargo.toml")"
lock_before="$(checksum "$repository/Cargo.lock")"
if run_release "$repository" patch false >/dev/null 2>&1; then
  fail 'missing Actions signing secret was accepted'
fi
assert_eq "$manifest_before" "$(checksum "$repository/Cargo.toml")" \
  'missing signing secret changed Cargo.toml'
assert_eq "$lock_before" "$(checksum "$repository/Cargo.lock")" \
  'missing signing secret changed Cargo.lock'
assert_clean "$repository"
pass 'GPG_PRIVATE_KEY Actions secret must exist before mutation'

repository="$(create_fixture)"
manifest_before="$(checksum "$repository/Cargo.toml")"
lock_before="$(checksum "$repository/Cargo.lock")"
if run_release "$repository" patch error >/dev/null 2>&1; then
  fail 'failed Actions secret query was accepted'
fi
assert_eq "$manifest_before" "$(checksum "$repository/Cargo.toml")" \
  'failed signing-secret query changed Cargo.toml'
assert_eq "$lock_before" "$(checksum "$repository/Cargo.lock")" \
  'failed signing-secret query changed Cargo.lock'
assert_clean "$repository"
pass 'Actions secret query failures stop before mutation'

printf 'All release helper contract tests passed.\n'
