#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
INSTALL_DIR="${HOME}/.local/opt/codex-cli"
EXPECTED_VERSION="$(node -p "require('${ROOT_DIR}/package.json').devDependencies['@openai/codex']")"

mkdir -p "${INSTALL_DIR}"
npm install --global --no-audit --no-fund --prefix "${INSTALL_DIR}" "@openai/codex@${EXPECTED_VERSION}"
