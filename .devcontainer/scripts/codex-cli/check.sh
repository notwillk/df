#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PACKAGE_JSON="${HOME}/.local/opt/codex-cli/lib/node_modules/@openai/codex/package.json"
EXPECTED_VERSION="$(node -p "require('${ROOT_DIR}/package.json').devDependencies['@openai/codex']")"

if [ ! -x "${HOME}/.local/opt/codex-cli/bin/codex" ]; then
  exit 1
fi

if [ ! -f "${PACKAGE_JSON}" ]; then
  exit 1
fi

ACTUAL_VERSION="$(node -p "require('${PACKAGE_JSON}').version")"
test "${ACTUAL_VERSION}" = "${EXPECTED_VERSION}"
