#!/bin/sh

set -eu

REPOSITORY="notwillk/df"
DEFAULT_DEST="/usr/local/bin"

fail() {
  printf 'dof installer: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

download() {
  destination=$1
  url=$2

  curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
    --output "$destination" "$url"
}

is_release_tag() {
  printf '%s\n' "$1" | grep -Eq \
    '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
}

resolve_version() {
  requested_version=$1

  if [ "$requested_version" != "latest" ]; then
    is_release_tag "$requested_version" || \
      fail "DOF_VERSION must be 'latest' or an exact vX.Y.Z tag"
    printf '%s\n' "$requested_version"
    return
  fi

  latest_json=$temporary_directory/latest.json
  download "$latest_json" "https://api.github.com/repos/$REPOSITORY/releases/latest" || \
    fail "could not determine the latest release"

  latest_tag=$(sed -n \
    's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$latest_json" | head -n 1)
  is_release_tag "$latest_tag" || \
    fail "latest GitHub release did not contain an exact vX.Y.Z tag"
  printf '%s\n' "$latest_tag"
}

select_archive() {
  operating_system=$1
  architecture=$2

  case "$operating_system:$architecture" in
    Linux:x86_64 | Linux:amd64)
      printf '%s\n' 'dof_linux_x86_64.tar.gz'
      ;;
    Linux:aarch64 | Linux:arm64)
      printf '%s\n' 'dof_linux_aarch64.tar.gz'
      ;;
    Darwin:arm64 | Darwin:aarch64)
      printf '%s\n' 'dof_darwin_aarch64.tar.gz'
      ;;
    Darwin:x86_64 | Darwin:amd64)
      fail "Intel macOS is not supported"
      ;;
    *)
      fail "unsupported platform: $operating_system $architecture"
      ;;
  esac
}

sha256_file() {
  file=$1

  if [ "$detected_os" = "Darwin" ]; then
    require_command shasum
    shasum -a 256 "$file" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{ print $1 }'
  else
    fail "required SHA-256 command not found (sha256sum or shasum)"
  fi
}

manifest_checksum() {
  manifest=$1
  archive=$2

  awk -v archive="$archive" '
    NF == 2 && $2 == archive {
      matches += 1
      checksum = $1
    }
    END {
      if (matches != 1) {
        exit 1
      }
      print checksum
    }
  ' "$manifest"
}

install_binary() {
  source_binary=$1
  destination_directory=$2

  if [ -e "$destination_directory" ] && [ ! -d "$destination_directory" ]; then
    fail "DEST is not a directory: $destination_directory"
  fi

  if [ -d "$destination_directory" ] && [ -w "$destination_directory" ]; then
    install -m 0755 "$source_binary" "$destination_directory/dof"
    return
  fi

  if [ ! -e "$destination_directory" ] && \
    mkdir -p "$destination_directory" 2>/dev/null; then
    install -m 0755 "$source_binary" "$destination_directory/dof"
    return
  fi

  require_command sudo
  sudo install -d -m 0755 "$destination_directory"
  sudo install -m 0755 "$source_binary" "$destination_directory/dof"
}

main() {
  detected_os=$(uname -s)
  detected_architecture=$(uname -m)
  archive_name=$(select_archive "$detected_os" "$detected_architecture")

  require_command curl
  require_command gpg
  require_command grep
  require_command awk
  require_command sed
  require_command tar
  require_command install
  require_command mktemp

  umask 077
  temporary_directory=$(mktemp -d "${TMPDIR:-/tmp}/dof-install.XXXXXX") || \
    fail "could not create a temporary directory"
  trap 'rm -rf "$temporary_directory"' EXIT HUP INT TERM

  release_tag=$(resolve_version "${DOF_VERSION:-latest}")
  destination_directory=${DEST:-$DEFAULT_DEST}

  release_url="https://github.com/$REPOSITORY/releases/download/$release_tag"
  source_url="https://raw.githubusercontent.com/$REPOSITORY/$release_tag"
  archive_path=$temporary_directory/$archive_name
  checksums_path=$temporary_directory/checksums.txt
  signature_path=$temporary_directory/checksums.txt.sig
  public_key_path=$temporary_directory/signing-key.asc

  download "$archive_path" "$release_url/$archive_name" || \
    fail "could not download $archive_name"
  download "$checksums_path" "$release_url/checksums.txt" || \
    fail "could not download checksums.txt"
  download "$signature_path" "$release_url/checksums.txt.sig" || \
    fail "could not download checksums.txt.sig"
  download "$public_key_path" "$source_url/keys/signing-key.asc" || \
    fail "could not download the release-tagged signing key"

  gpg_home=$temporary_directory/gnupg
  mkdir -m 0700 "$gpg_home"
  gpg --batch --quiet --homedir "$gpg_home" --import "$public_key_path" || \
    fail "could not import the release signing key"
  gpg --batch --quiet --homedir "$gpg_home" \
    --verify "$signature_path" "$checksums_path" || \
    fail "checksum signature verification failed"

  expected_checksum=$(manifest_checksum "$checksums_path" "$archive_name") || \
    fail "checksums.txt does not contain exactly one entry for $archive_name"
  printf '%s\n' "$expected_checksum" | grep -Eq '^[[:xdigit:]]{64}$' || \
    fail "invalid SHA-256 value for $archive_name"

  actual_checksum=$(sha256_file "$archive_path") || \
    fail "could not calculate the archive checksum"
  expected_checksum=$(printf '%s' "$expected_checksum" | tr 'A-F' 'a-f')
  actual_checksum=$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')
  [ "$actual_checksum" = "$expected_checksum" ] || \
    fail "checksum mismatch for $archive_name"

  archive_listing=$(tar -tzf "$archive_path") || fail "could not inspect $archive_name"
  [ "$archive_listing" = "dof" ] || \
    fail "release archive must contain exactly one top-level executable named dof"

  staging_directory=$temporary_directory/staging
  mkdir "$staging_directory"
  tar -xzf "$archive_path" -C "$staging_directory" || \
    fail "could not extract $archive_name"
  [ -f "$staging_directory/dof" ] && [ ! -L "$staging_directory/dof" ] && \
    [ -x "$staging_directory/dof" ] || \
    fail "release archive does not contain a regular executable dof binary"

  install_binary "$staging_directory/dof" "$destination_directory"
  printf 'Installed dof %s to %s/dof\n' "$release_tag" "$destination_directory"
}

main "$@"
