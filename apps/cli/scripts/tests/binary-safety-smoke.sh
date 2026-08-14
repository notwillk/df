#!/usr/bin/env bash

set -Eeuo pipefail

fail() {
  printf 'binary safety smoke: %s\n' "$*" >&2
  exit 1
}

file_mode() {
  local path=$1
  local mode
  if mode="$(stat -c '%a' "$path" 2>/dev/null)"; then
    printf '%s\n' "$mode"
    return
  fi
  stat -f '%Lp' "$path"
}

[[ $# -eq 1 ]] || fail 'usage: binary-safety-smoke.sh <dof-binary>'

binary_input=$1
binary_dir="$(CDPATH= cd -- "$(dirname -- "$binary_input")" && pwd -P)"
binary="$binary_dir/$(basename -- "$binary_input")"
[[ -f $binary && ! -L $binary && -x $binary ]] ||
  fail "not a regular executable: $binary"
command -v git >/dev/null 2>&1 || fail 'git is required'
command -v stat >/dev/null 2>&1 || fail 'stat is required'

test_root="$(mktemp -d "${TMPDIR:-/tmp}/dof-binary-safety.XXXXXX")"
cleanup() {
  local status=$?
  trap - EXIT
  rm -rf -- "$test_root"
  exit "$status"
}
trap cleanup EXIT

home=$test_root/home
repository=$test_root/repository
mkdir -p \
  "$home/existing-directory" \
  "$repository/features/default/home" \
  "$repository/features/hostname/home" \
  "$repository/features/macos-gui/home/existing-directory" \
  "$repository/features/macos-gui/home/optional-new-directory"

printf 'default applied\n' >"$repository/features/default/home/default-applied.txt"
printf 'dangerous hostname mutation\n' >"$repository/features/hostname/home/hostname.txt"
printf 'dangerous nested mutation\n' \
  >"$repository/features/macos-gui/home/existing-directory/nested.txt"
printf 'dangerous directory mutation\n' \
  >"$repository/features/macos-gui/home/optional-new-directory/resource.txt"
printf '%s\n' \
  'snippets:' \
  '  .profile:' \
  "    - 'dangerous optional snippet'" \
  >"$repository/features/macos-gui/snippets.yaml"
printf '%s\n' \
  '#!/bin/sh' \
  'printf "optional hook ran\\n" >"$HOME/optional-hook-ran"' \
  >"$repository/features/macos-gui/apply"
chmod 0755 "$repository/features/macos-gui/apply"

printf 'original hostname\n' >"$test_root/hostname.expected"
printf 'original nested file\n' >"$test_root/nested.expected"
printf 'original profile\n' >"$test_root/profile.expected"
cp "$test_root/hostname.expected" "$home/hostname.txt"
cp "$test_root/nested.expected" "$home/existing-directory/nested.txt"
cp "$test_root/profile.expected" "$home/.profile"
chmod 0640 "$home/hostname.txt"
chmod 0600 "$home/existing-directory/nested.txt"
chmod 0644 "$home/.profile"
chmod 0750 "$home/existing-directory"

hostname_inode_before="$(ls -di "$home/hostname.txt" | awk '{ print $1 }')"
nested_inode_before="$(ls -di "$home/existing-directory/nested.txt" | awk '{ print $1 }')"
profile_inode_before="$(ls -di "$home/.profile" | awk '{ print $1 }')"
directory_inode_before="$(ls -di "$home/existing-directory" | awk '{ print $1 }')"
hostname_mode_before="$(file_mode "$home/hostname.txt")"
nested_mode_before="$(file_mode "$home/existing-directory/nested.txt")"
profile_mode_before="$(file_mode "$home/.profile")"
directory_mode_before="$(file_mode "$home/existing-directory")"

git -C "$repository" init -q
git -C "$repository" symbolic-ref HEAD refs/heads/main
git -C "$repository" config user.name 'Binary Safety Smoke'
git -C "$repository" config user.email 'binary-safety@example.invalid'
git -C "$repository" add .
git -C "$repository" -c core.hooksPath=/dev/null commit -q -m 'Safety fixture'

HOME="$home" "$binary" clone "file://$repository" >"$test_root/clone.out"
grep -Fx 'features: {}' "$home/.dof/config.yaml" >/dev/null ||
  fail 'clone did not write an empty feature map'
workspace=$home/.dof/workspace
[[ -f $workspace/features/hostname/home/hostname.txt ]] ||
  fail 'clone did not contain the omitted file resource'
[[ -f $workspace/features/macos-gui/home/existing-directory/nested.txt ]] ||
  fail 'clone did not contain the omitted nested directory resource'
[[ -f $workspace/features/macos-gui/home/optional-new-directory/resource.txt ]] ||
  fail 'clone did not contain the omitted new directory resource'
[[ -f $workspace/features/macos-gui/snippets.yaml ]] ||
  fail 'clone did not contain the omitted snippet resource'
[[ -x $workspace/features/macos-gui/apply ]] ||
  fail 'clone did not contain the omitted executable hook'

enabled="$(HOME="$home" "$binary" features --json)"
[[ $enabled == '["default"]' ]] ||
  fail "expected exact default-only listing, found: $enabled"

apply_output="$(HOME="$home" "$binary" apply)"
[[ $apply_output == $'applied: 1\nunchanged: 0' ]] ||
  fail "unexpected apply summary: $apply_output"

[[ $(<"$home/default-applied.txt") == 'default applied' ]] ||
  fail 'the enabled default resource was not applied'
cmp -s "$test_root/hostname.expected" "$home/hostname.txt" ||
  fail 'the omitted hostname file changed'
cmp -s "$test_root/nested.expected" "$home/existing-directory/nested.txt" ||
  fail 'the omitted nested file changed'
cmp -s "$test_root/profile.expected" "$home/.profile" ||
  fail 'the omitted snippet target changed'
[[ ! -e $home/optional-new-directory ]] ||
  fail 'the omitted directory resource was created'
[[ ! -e $home/optional-hook-ran ]] ||
  fail 'the omitted apply hook ran'
[[ ! -e $home/.dof/backups ]] ||
  fail 'omitted optional resources created a backup snapshot'

[[ $(ls -di "$home/hostname.txt" | awk '{ print $1 }') == "$hostname_inode_before" ]] ||
  fail 'the omitted hostname destination identity changed'
[[ $(ls -di "$home/existing-directory/nested.txt" | awk '{ print $1 }') == "$nested_inode_before" ]] ||
  fail 'the omitted nested destination identity changed'
[[ $(ls -di "$home/.profile" | awk '{ print $1 }') == "$profile_inode_before" ]] ||
  fail 'the omitted snippet destination identity changed'
[[ $(ls -di "$home/existing-directory" | awk '{ print $1 }') == "$directory_inode_before" ]] ||
  fail 'the omitted directory destination identity changed'
[[ $(file_mode "$home/hostname.txt") == "$hostname_mode_before" ]] ||
  fail 'the omitted hostname destination mode changed'
[[ $(file_mode "$home/existing-directory/nested.txt") == "$nested_mode_before" ]] ||
  fail 'the omitted nested destination mode changed'
[[ $(file_mode "$home/.profile") == "$profile_mode_before" ]] ||
  fail 'the omitted snippet destination mode changed'
[[ $(file_mode "$home/existing-directory") == "$directory_mode_before" ]] ||
  fail 'the omitted directory destination mode changed'

printf 'Binary safety smoke passed for %s.\n' "$binary"
