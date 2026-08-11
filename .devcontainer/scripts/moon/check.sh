#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN_PATH="${HOME}/.local/bin/moon"
PACKAGE_JSON="${HOME}/.local/lib/node_modules/@moonrepo/cli/package.json"
EXPECTED_VERSION="$(node -p "require('${ROOT_DIR}/package.json').devDependencies['@moonrepo/cli']")"

if [ ! -x "${BIN_PATH}" ]; then
  exit 1
fi

if [ ! -f "${PACKAGE_JSON}" ]; then
  exit 1
fi

ACTUAL_VERSION="$(node -p "require('${PACKAGE_JSON}').version")"
test "${ACTUAL_VERSION}" = "${EXPECTED_VERSION}"
