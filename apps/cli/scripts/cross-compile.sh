#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <target>" >&2
}

fail() {
  echo "Release build failed: $*" >&2
  exit 1
}

if [[ $# -ne 1 ]]; then
  usage
  exit 2
fi

target="$1"
case "$target" in
  x86_64-unknown-linux-musl)
    artifact_stem="dof_linux_x86_64"
    platform="linux"
    ;;
  aarch64-unknown-linux-musl)
    artifact_stem="dof_linux_aarch64"
    platform="linux"
    ;;
  aarch64-apple-darwin)
    artifact_stem="dof_darwin_aarch64"
    platform="darwin"
    ;;
  *)
    fail "unsupported target '$target'"
    ;;
esac

for command_name in cargo tar install; do
  command -v "$command_name" >/dev/null 2>&1 || fail "required command '$command_name' is unavailable"
done

if command -v sha256sum >/dev/null 2>&1; then
  checksum_command=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
  checksum_command=(shasum -a 256)
else
  fail "neither sha256sum nor shasum is available"
fi

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"
workspace_root="$(CDPATH= cd -- "$project_root/../.." && pwd)"
dist_dir="$project_root/dist"
archive_name="$artifact_stem.tar.gz"
checksum_name="$artifact_stem-checksum.txt"
binary="$workspace_root/target/$target/release/dof"
cross_config="$project_root/Cross.toml"

mkdir -p "$dist_dir"

if [[ $platform == darwin ]]; then
  [[ $(uname -s) == Darwin ]] || fail "$target must be built natively on macOS"
  [[ $(uname -m) == arm64 ]] || fail "$target must be built on an Apple Silicon runner"
  command -v rustup >/dev/null 2>&1 || fail "required command 'rustup' is unavailable"
  rustup target add "$target"
  cargo build \
    --manifest-path "$workspace_root/Cargo.toml" \
    --package dof \
    --bin dof \
    --locked \
    --release \
    --target "$target"
else
  installed_cross_version="$(cross --version 2>/dev/null | sed -n '1p' || true)"
  if [[ $installed_cross_version != "cross 0.2.5" ]]; then
    cargo install cross --version 0.2.5 --locked --force
  fi
  (
    cd "$workspace_root"
    CROSS_CONFIG="$cross_config" cross build \
      --package dof \
      --bin dof \
      --locked \
      --release \
      --target "$target"
  )
fi

[[ -f $binary ]] || fail "build did not produce $binary"
[[ -x $binary ]] || fail "built binary is not executable: $binary"

staging_dir="$(mktemp -d "$dist_dir/.package.XXXXXX")"
cleanup() {
  rm -rf -- "$staging_dir"
}
trap cleanup EXIT

install -m 0755 "$binary" "$staging_dir/dof"
tar -czf "$dist_dir/$archive_name" -C "$staging_dir" dof
(
  cd "$dist_dir"
  "${checksum_command[@]}" "$archive_name"
) >"$dist_dir/$checksum_name"

printf 'Created %s\n' "$dist_dir/$archive_name"
printf 'Created %s\n' "$dist_dir/$checksum_name"
