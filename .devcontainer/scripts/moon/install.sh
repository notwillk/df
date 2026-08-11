#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
EXPECTED_VERSION="$(node -p "require('${ROOT_DIR}/package.json').devDependencies['@moonrepo/cli']")"

mkdir -p "${HOME}/.local"
npm install --global --no-audit --no-fund --prefix "${HOME}/.local" "@moonrepo/cli@${EXPECTED_VERSION}"
