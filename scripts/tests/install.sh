#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
WORKSPACE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
INSTALLER=$WORKSPACE_ROOT/scripts/install.sh
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dof-install-contract.XXXXXX")
trap 'rm -rf "$TEST_ROOT"' EXIT HUP INT TERM

PASS_COUNT=0
FAIL_COUNT=0

pass() {
  PASS_COUNT=$((PASS_COUNT + 1))
  printf 'ok %d - %s\n' "$PASS_COUNT" "$1"
}

fail() {
  FAIL_COUNT=$((FAIL_COUNT + 1))
  printf 'not ok - %s\n' "$1" >&2
}

assert_contains() {
  file=$1
  expected=$2
  description=$3

  if grep -F "$expected" "$file" >/dev/null 2>&1; then
    pass "$description"
  else
    fail "$description (missing: $expected)"
  fi
}

assert_not_contains() {
  file=$1
  unexpected=$2
  description=$3

  if grep -F "$unexpected" "$file" >/dev/null 2>&1; then
    fail "$description (found: $unexpected)"
  else
    pass "$description"
  fi
}

make_mock() {
  path=$1
  shift
  {
    printf '%s\n' '#!/bin/sh' 'set -eu'
    printf '%s\n' "$@"
  } >"$path"
  chmod +x "$path"
}

setup_case() {
  case_name=$1
  CASE_ROOT=$TEST_ROOT/$case_name
  MOCK_BIN=$CASE_ROOT/bin
  FIXTURES=$CASE_ROOT/fixtures
  LOG=$CASE_ROOT/commands.log
  mkdir -p "$MOCK_BIN" "$FIXTURES" "$CASE_ROOT/dest" "$CASE_ROOT/archive"
  : >"$LOG"

  printf '%s\n' '#!/bin/sh' 'exit 0' >"$CASE_ROOT/archive/dof"
  chmod +x "$CASE_ROOT/archive/dof"
  tar -czf "$FIXTURES/archive.tar.gz" -C "$CASE_ROOT/archive" dof

  make_mock "$MOCK_BIN/uname" '
case "$1" in
  -s) printf "%s\n" "${MOCK_OS:-Linux}" ;;
  -m) printf "%s\n" "${MOCK_ARCH:-x86_64}" ;;
  *) exit 2 ;;
esac'

  make_mock "$MOCK_BIN/curl" '
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output=$2; shift 2 ;;
    --*) shift ;;
    *) url=$1; shift ;;
  esac
done
printf "curl %s\n" "$url" >>"$MOCK_LOG"
case "$url" in
  */releases/latest)
    printf "{\"tag_name\":\"%s\"}\n" "${MOCK_LATEST_TAG:-v1.2.3}" >"$output"
    ;;
  */dof_linux_x86_64.tar.gz|*/dof_linux_aarch64.tar.gz|*/dof_darwin_aarch64.tar.gz)
    cp "$MOCK_FIXTURES/archive.tar.gz" "$output"
    ;;
  */checksums.txt)
    if [ "${MOCK_MISSING_CHECKSUM:-0}" = 1 ]; then
      printf "%s  something-else.tar.gz\n" "$MOCK_ARCHIVE_SHA" >"$output"
    else
      archive=${url##*/}
      case "$MOCK_OS:${MOCK_ARCH}" in
        Linux:x86_64|Linux:amd64) archive=dof_linux_x86_64.tar.gz ;;
        Linux:aarch64|Linux:arm64) archive=dof_linux_aarch64.tar.gz ;;
        Darwin:arm64|Darwin:aarch64) archive=dof_darwin_aarch64.tar.gz ;;
      esac
      printf "%s  %s\n" "${MOCK_MANIFEST_SHA:-$MOCK_ARCHIVE_SHA}" "$archive" >"$output"
    fi
    ;;
  */checksums.txt.sig)
    printf "signature\n" >"$output"
    ;;
  */keys/signing-key.asc)
    [ "${MOCK_KEY_DOWNLOAD_FAIL:-0}" != 1 ] || exit 22
    printf "public key\n" >"$output"
    ;;
  *)
    exit 22
    ;;
esac'

  make_mock "$MOCK_BIN/gpg" '
printf "gpg %s\n" "$*" >>"$MOCK_LOG"
case " $* " in
  *" --import "*) [ "${MOCK_GPG_IMPORT_FAIL:-0}" != 1 ] || exit 1 ;;
  *" --verify "*) [ "${MOCK_GPG_VERIFY_FAIL:-0}" != 1 ] || exit 1 ;;
esac'

  make_mock "$MOCK_BIN/sha256sum" '
printf "sha256sum %s\n" "$*" >>"$MOCK_LOG"
printf "%s  %s\n" "${MOCK_ACTUAL_SHA:-$MOCK_ARCHIVE_SHA}" "$1"'

  make_mock "$MOCK_BIN/shasum" '
printf "shasum %s\n" "$*" >>"$MOCK_LOG"
last=
for argument in "$@"; do last=$argument; done
printf "%s  %s\n" "${MOCK_ACTUAL_SHA:-$MOCK_ARCHIVE_SHA}" "$last"'

  make_mock "$MOCK_BIN/install" '
printf "install %s\n" "$*" >>"$MOCK_LOG"'

  make_mock "$MOCK_BIN/sudo" '
printf "sudo %s\n" "$*" >>"$MOCK_LOG"
"$@"'
}

run_installer() {
  stdout=$1
  stderr=$2
  shift 2

  env \
    PATH="$MOCK_BIN:/usr/bin:/bin" \
    TMPDIR="$CASE_ROOT/tmp" \
    MOCK_LOG="$LOG" \
    MOCK_FIXTURES="$FIXTURES" \
    MOCK_ARCHIVE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
    "$@" \
    sh "$INSTALLER" >"$stdout" 2>"$stderr"
}

expect_success() {
  description=$1
  shift
  mkdir -p "$CASE_ROOT/tmp"
  if run_installer "$CASE_ROOT/stdout" "$CASE_ROOT/stderr" "$@"; then
    pass "$description"
  else
    fail "$description"
    sed -n '1,80p' "$CASE_ROOT/stderr" >&2
  fi
}

expect_failure() {
  description=$1
  shift
  mkdir -p "$CASE_ROOT/tmp"
  if run_installer "$CASE_ROOT/stdout" "$CASE_ROOT/stderr" "$@"; then
    fail "$description (unexpected success)"
  else
    pass "$description"
  fi
}

setup_case linux_x86_latest
expect_success "Linux x86-64 latest install succeeds" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DEST="$CASE_ROOT/dest"
assert_contains "$LOG" \
  '/releases/download/v1.2.3/dof_linux_x86_64.tar.gz' \
  "latest release selects the Linux x86-64 archive"
assert_contains "$LOG" "/v1.2.3/keys/signing-key.asc" \
  "latest release downloads its tagged public key"
assert_contains "$LOG" "install -m 0755" \
  "writable destination installs without sudo"
assert_not_contains "$LOG" "sudo " \
  "writable destination never invokes sudo"

setup_case linux_arm_explicit
expect_success "Linux ARM64 explicit version succeeds" \
  MOCK_OS=Linux MOCK_ARCH=aarch64 DOF_VERSION=v2.3.4 DEST="$CASE_ROOT/dest"
assert_contains "$LOG" "/v2.3.4/dof_linux_aarch64.tar.gz" \
  "explicit release selects the Linux ARM64 archive"
assert_not_contains "$LOG" "/releases/latest" \
  "explicit release does not query the latest release"

setup_case darwin_arm
expect_success "Apple Silicon macOS install succeeds" \
  MOCK_OS=Darwin MOCK_ARCH=arm64 DOF_VERSION=v3.4.5 DEST="$CASE_ROOT/dest"
assert_contains "$LOG" "/v3.4.5/dof_darwin_aarch64.tar.gz" \
  "Apple Silicon selects the Darwin ARM64 archive"
assert_contains "$LOG" "shasum -a 256" \
  "macOS uses its portable SHA-256 command"

setup_case destination_override
custom_dest=$CASE_ROOT/custom/bin
mkdir -p "$custom_dest"
expect_success "DEST override succeeds" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$custom_dest"
assert_contains "$LOG" "$custom_dest/dof" \
  "DEST override controls the installed path"

setup_case default_destination
expect_success "default destination succeeds" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0
assert_contains "$LOG" "/usr/local/bin/dof" \
  "the default destination is /usr/local/bin"

setup_case sudo_destination
expect_success "non-writable destination uses sudo" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 \
  DEST="/proc/dof-installer-contract-$$"
assert_contains "$LOG" "sudo install -d -m 0755 /proc/dof-installer-contract-$$" \
  "sudo creates a destination that cannot be written directly"
assert_contains "$LOG" "sudo install -m 0755" \
  "sudo installs into a destination that cannot be written directly"

setup_case intel_mac
expect_failure "Intel macOS is rejected" \
  MOCK_OS=Darwin MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest"
assert_contains "$CASE_ROOT/stderr" "Intel macOS is not supported" \
  "Intel macOS rejection is explicit"
assert_not_contains "$LOG" "curl " \
  "unsupported platform is rejected before download"

setup_case intel_mac_without_tools
rm "$MOCK_BIN/gpg" "$MOCK_BIN/curl"
if env PATH="$MOCK_BIN" MOCK_OS=Darwin MOCK_ARCH=x86_64 \
  MOCK_LOG="$LOG" /bin/sh "$INSTALLER" \
  >"$CASE_ROOT/stdout" 2>"$CASE_ROOT/stderr"; then
  fail "Intel macOS without installer dependencies is rejected (unexpected success)"
else
  pass "Intel macOS without installer dependencies is rejected"
fi
assert_contains "$CASE_ROOT/stderr" "Intel macOS is not supported" \
  "Intel macOS is diagnosed before missing GPG or curl"
assert_not_contains "$CASE_ROOT/stderr" "required command not found" \
  "unsupported-platform rejection precedes dependency checks"
assert_not_contains "$LOG" "curl " \
  "Intel macOS without dependencies performs no downloads"

setup_case unsupported_system
expect_failure "unsupported systems are rejected" \
  MOCK_OS=FreeBSD MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest"
assert_contains "$CASE_ROOT/stderr" "unsupported platform: FreeBSD x86_64" \
  "unsupported-system diagnostic includes the platform"

setup_case invalid_version
expect_failure "invalid explicit version is rejected" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=1.2.3 DEST="$CASE_ROOT/dest"
assert_contains "$CASE_ROOT/stderr" "DOF_VERSION must be 'latest' or an exact vX.Y.Z tag" \
  "invalid-version diagnostic explains the contract"

setup_case missing_key
expect_failure "missing release signing key fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest" \
  MOCK_KEY_DOWNLOAD_FAIL=1
assert_contains "$CASE_ROOT/stderr" "could not download the release-tagged signing key" \
  "missing-key diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "missing key prevents installation"

setup_case key_import_failure
expect_failure "unimportable release signing key fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest" \
  MOCK_GPG_IMPORT_FAIL=1
assert_contains "$CASE_ROOT/stderr" "could not import the release signing key" \
  "key-import diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "key import failure prevents installation"

setup_case bad_signature
expect_failure "invalid signature fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest" \
  MOCK_GPG_VERIFY_FAIL=1
assert_contains "$CASE_ROOT/stderr" "checksum signature verification failed" \
  "signature failure diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "signature failure prevents installation"

setup_case missing_checksum
expect_failure "missing checksum entry fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest" \
  MOCK_MISSING_CHECKSUM=1
assert_contains "$CASE_ROOT/stderr" "does not contain exactly one entry" \
  "missing checksum diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "missing checksum prevents installation"

setup_case checksum_mismatch
expect_failure "checksum mismatch fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest" \
  MOCK_ACTUAL_SHA=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
assert_contains "$CASE_ROOT/stderr" "checksum mismatch" \
  "checksum mismatch diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "checksum mismatch prevents installation"

setup_case symlink_archive
rm "$CASE_ROOT/archive/dof"
ln -s /bin/sh "$CASE_ROOT/archive/dof"
tar -czf "$FIXTURES/archive.tar.gz" -C "$CASE_ROOT/archive" dof
expect_failure "symlink binary archive fails closed" \
  MOCK_OS=Linux MOCK_ARCH=x86_64 DOF_VERSION=v1.0.0 DEST="$CASE_ROOT/dest"
assert_contains "$CASE_ROOT/stderr" \
  "does not contain a regular executable dof binary" \
  "symlink archive diagnostic is explicit"
assert_not_contains "$LOG" "install -m 0755" \
  "symlink archive prevents installation"

if [ "$FAIL_COUNT" -ne 0 ]; then
  printf '%d installer contract assertion(s) failed\n' "$FAIL_COUNT" >&2
  exit 1
fi

printf 'All %d installer contract assertions passed.\n' "$PASS_COUNT"
