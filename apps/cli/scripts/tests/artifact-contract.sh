#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(CDPATH= cd -- "$script_dir/../.." && pwd)"
workspace_root="$(CDPATH= cd -- "$project_root/../.." && pwd)"
cross_compile="$project_root/scripts/cross-compile.sh"
linux_verify="$project_root/scripts/verify-static-linux-binary.sh"
macos_verify="$project_root/scripts/verify-macos-binary.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/dof-artifact-contract.XXXXXX")"

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

assert_contains() {
  local file="$1"
  local expected="$2"
  local description="$3"
  grep -F -- "$expected" "$file" >/dev/null ||
    fail "$description (missing '$expected')"
}

link_tool() {
  local destination="$1"
  local name="$2"
  local source
  source="$(type -P "$name")"
  [[ -n $source ]] || fail "test prerequisite '$name' is unavailable"
  ln -s "$source" "$destination/$name"
}

fixture="$test_root/package"
mock_bin="$fixture/mock-bin"
mkdir -p "$fixture/apps/cli/scripts" "$mock_bin"
cp "$cross_compile" "$fixture/apps/cli/scripts/cross-compile.sh"
cp "$project_root/Cross.toml" "$fixture/apps/cli/Cross.toml"
chmod +x "$fixture/apps/cli/scripts/cross-compile.sh"
printf '%s\n' '[workspace]' >"$fixture/Cargo.toml"

for tool in bash chmod dirname gzip install mkdir mktemp rm sed tar; do
  link_tool "$mock_bin" "$tool"
done

cat >"$mock_bin/cross" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == --version ]]; then
  printf 'cross 0.2.5\n'
  exit 0
fi
printf 'cross CROSS_CONFIG=%s %s\n' "${CROSS_CONFIG:-}" "$*" >>"$MOCK_LOG"
[[ ${CROSS_CONFIG:-} == "$MOCK_PROJECT_ROOT/Cross.toml" ]]
target=
while [[ $# -gt 0 ]]; do
  if [[ $1 == --target ]]; then
    target="$2"
    break
  fi
  shift
done
[[ -n $target ]]
mkdir -p "$MOCK_WORKSPACE_ROOT/target/$target/release"
printf '#!/usr/bin/env sh\nexit 0\n' >"$MOCK_WORKSPACE_ROOT/target/$target/release/dof"
chmod +x "$MOCK_WORKSPACE_ROOT/target/$target/release/dof"
EOF

cat >"$mock_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "$*" >>"$MOCK_LOG"
target=
while [[ $# -gt 0 ]]; do
  if [[ $1 == --target ]]; then
    target="$2"
    break
  fi
  shift
done
[[ -n $target ]]
mkdir -p "$MOCK_WORKSPACE_ROOT/target/$target/release"
printf '#!/usr/bin/env sh\nexit 0\n' >"$MOCK_WORKSPACE_ROOT/target/$target/release/dof"
chmod +x "$MOCK_WORKSPACE_ROOT/target/$target/release/dof"
EOF

cat >"$mock_bin/rustup" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'rustup %s\n' "$*" >>"$MOCK_LOG"
EOF

cat >"$mock_bin/uname" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -s) printf '%s\n' "${MOCK_OS:-Linux}" ;;
  -m) printf '%s\n' "${MOCK_ARCH:-x86_64}" ;;
  *) exit 2 ;;
esac
EOF

cat >"$mock_bin/shasum" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'shasum %s\n' "$*" >>"$MOCK_LOG"
last=
for argument in "$@"; do
  last="$argument"
done
printf '%064d  %s\n' 0 "$last"
EOF

chmod +x "$mock_bin/cross" "$mock_bin/cargo" "$mock_bin/rustup" "$mock_bin/uname" "$mock_bin/shasum"

run_packaging_case() {
  local target="$1"
  local artifact_stem="$2"
  local os="$3"
  local arch="$4"
  local log="$fixture/$target.log"
  local dist="$fixture/apps/cli/dist"
  local extract="$fixture/extract"

  rm -rf -- "$dist" "$extract" "$fixture/target"
  mkdir -p "$extract"
  : >"$log"
  env \
    PATH="$mock_bin" \
    MOCK_LOG="$log" \
    MOCK_OS="$os" \
    MOCK_ARCH="$arch" \
    MOCK_PROJECT_ROOT="$fixture/apps/cli" \
    MOCK_WORKSPACE_ROOT="$fixture" \
    "$fixture/apps/cli/scripts/cross-compile.sh" "$target" >/dev/null

  [[ -f $dist/$artifact_stem.tar.gz ]] || fail "$target archive name is incorrect"
  [[ -f $dist/$artifact_stem-checksum.txt ]] || fail "$target checksum name is incorrect"
  [[ $(find "$dist" -mindepth 1 -maxdepth 1 -type f | wc -l | tr -d ' ') == 2 ]] ||
    fail "$target produced unexpected release files"
  [[ $(tar -tzf "$dist/$artifact_stem.tar.gz") == dof ]] ||
    fail "$target archive does not contain exactly dof"
  tar -xzf "$dist/$artifact_stem.tar.gz" -C "$extract"
  [[ -x $extract/dof ]] || fail "$target archive did not preserve executable mode"
  grep -Eq "^0{64}  ${artifact_stem}\.tar\.gz$" "$dist/$artifact_stem-checksum.txt" ||
    fail "$target checksum fragment has an unexpected format"
  assert_contains "$log" "shasum -a 256" "$target uses the portable shasum fallback"

  if [[ $os == Linux ]]; then
    assert_contains "$log" "CROSS_CONFIG=$fixture/apps/cli/Cross.toml" \
      "$target explicitly selects the pinned Cross configuration"
    assert_contains "$log" "--locked --release --target $target" \
      "$target build is locked"
  else
    assert_contains "$log" "rustup target add $target" \
      "$target installs the Rust target"
    assert_contains "$log" "--locked --release --target $target" \
      "$target native build is locked"
  fi

  pass "$target packaging contract"
}

run_packaging_case x86_64-unknown-linux-musl dof_linux_x86_64 Linux x86_64
run_packaging_case aarch64-unknown-linux-musl dof_linux_aarch64 Linux x86_64
run_packaging_case aarch64-apple-darwin dof_darwin_aarch64 Darwin arm64

rm -rf -- "$fixture/apps/cli/dist" "$fixture/target"
if env PATH="$mock_bin" "$fixture/apps/cli/scripts/cross-compile.sh" x86_64-apple-darwin \
  >"$fixture/unsupported.out" 2>&1
then
  fail 'unsupported target was accepted'
fi
assert_contains "$fixture/unsupported.out" "unsupported target 'x86_64-apple-darwin'" \
  'unsupported target error is contextual'
[[ ! -e $fixture/apps/cli/dist ]] || fail 'unsupported target created release output'
pass 'packaging rejects targets outside the three-target allowlist'

assert_contains "$cross_compile" 'cargo install cross --version 0.2.5 --locked --force' \
  'Cross installation is version pinned'
assert_contains "$workspace_root/.github/workflows/release.yml" \
  'cargo test --workspace --locked --no-fail-fast' \
  'release tests continue after an integration test binary fails'
assert_contains "$project_root/moon.yml" \
  'command: cargo test --workspace --locked --no-fail-fast' \
  'Moon tests continue after an integration test binary fails'
assert_contains "$project_root/scripts/release.sh" \
  '"$CARGO_BIN" test --workspace --all-features --locked --no-fail-fast' \
  'local release tests continue after an integration test binary fails'
assert_contains "$project_root/Cross.toml" \
  'x86_64-unknown-linux-musl@sha256:77db671d8356a64ae72a3e1415e63f547f26d374fbe3c4762c1cd36c7eac7b99' \
  'x86-64 musl image is digest pinned'
assert_contains "$project_root/Cross.toml" \
  'aarch64-unknown-linux-musl@sha256:702154f52b2d8091671aa2c84d5582d849f949977228c735ff8462f93cc0e1e4' \
  'ARM64 musl image is digest pinned'
pass 'Cross tool and images are pinned'

verify_fixture="$test_root/verify"
verify_bin="$verify_fixture/bin"
mkdir -p "$verify_bin"
for tool in bash grep mktemp rm sed; do
  link_tool "$verify_bin" "$tool"
done

cat >"$verify_fixture/dof" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version) printf 'dof 1.2.3\n' ;;
  --help) exit 0 ;;
  lint) [[ -d ${2:-} ]] ;;
  *) exit 2 ;;
esac
EOF

cat >"$verify_bin/uname" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -s) printf '%s\n' "${MOCK_OS:-Linux}" ;;
  -m) printf '%s\n' "${MOCK_ARCH:-x86_64}" ;;
  *) exit 2 ;;
esac
EOF

cat >"$verify_bin/readelf" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -h)
    printf '  Type:                              EXEC (Executable file)\n'
    printf '  Machine:                           Advanced Micro Devices X86-64\n'
    ;;
  -lW)
    if [[ ${MOCK_DYNAMIC:-0} == 1 ]]; then
      printf '  INTERP 0x000000\n'
    else
      printf 'Program Headers:\n'
    fi
    ;;
  -dW) printf 'There is no dynamic section in this file.\n' ;;
  --version-info) printf 'No version information found in this file.\n' ;;
  *) exit 2 ;;
esac
EOF

cat >"$verify_bin/file" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${MOCK_MAC_FORMAT:-valid} == valid ]]; then
  printf '%s: Mach-O 64-bit executable arm64\n' "$1"
else
  printf '%s: Mach-O 64-bit executable x86_64\n' "$1"
fi
EOF

cat >"$verify_bin/lipo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "${MOCK_LIPO_ARCHS:-arm64}"
EOF

chmod +x "$verify_fixture/dof" "$verify_bin/uname" "$verify_bin/readelf" "$verify_bin/file" "$verify_bin/lipo"

env PATH="$verify_bin" MOCK_OS=Linux MOCK_ARCH=x86_64 \
  "$linux_verify" "$verify_fixture/dof" 1.2.3 >/dev/null ||
  fail 'static Linux verifier rejected the valid mocked contract'
if env PATH="$verify_bin" MOCK_OS=Linux MOCK_ARCH=x86_64 MOCK_DYNAMIC=1 \
  "$linux_verify" "$verify_fixture/dof" 1.2.3 >/dev/null 2>&1
then
  fail 'static Linux verifier accepted an interpreter'
fi
pass 'static Linux verifier accepts static structure and rejects an interpreter'

env PATH="$verify_bin" MOCK_OS=Darwin MOCK_ARCH=arm64 MOCK_MAC_FORMAT=valid \
  "$macos_verify" "$verify_fixture/dof" 1.2.3 >/dev/null ||
  fail 'macOS verifier rejected the valid mocked ARM64 contract'
if env PATH="$verify_bin" MOCK_OS=Darwin MOCK_ARCH=arm64 MOCK_MAC_FORMAT=invalid \
  "$macos_verify" "$verify_fixture/dof" 1.2.3 >/dev/null 2>&1
then
  fail 'macOS verifier accepted an x86-64 artifact'
fi
pass 'macOS verifier accepts ARM64 structure and rejects x86-64'

current_version="$("$project_root/scripts/get-version.sh")"
"$project_root/scripts/ensure-tag-matches-version.sh" "v$current_version" >/dev/null ||
  fail 'current workspace tag/version contract failed'
if "$project_root/scripts/ensure-tag-matches-version.sh" v999.999.999 >/dev/null 2>&1; then
  fail 'mismatched release tag was accepted'
fi
pass 'release tag must exactly match the root workspace version'

printf 'All artifact contract tests passed.\n'
